use convert_case::{Case, Casing};
use proc_macro2::Ident;
use quote::quote;
use structmeta::{Flag, NameValue, StructMeta};
use syn::{parse_macro_input, spanned::Spanned, Attribute, LitStr};

#[derive(Clone, StructMeta, Default)]
struct FieldAttrs {
    primary_key: Flag,
    displayed: Flag,
    searchable: Flag,
    distinct: Flag,
    filterable: Flag,
    sortable: Flag,
}

#[derive(StructMeta)]
struct StructAttrs {
    index_name: Option<NameValue<LitStr>>,
    max_total_hits: Option<NameValue<syn::Expr>>,
}

fn is_valid_name(name: &str) -> bool {
    name.chars()
        .all(|c| matches!(c, 'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_'))
        && !name.is_empty()
}

#[proc_macro_derive(IndexConfig, attributes(index_config))]
pub fn generate_index_settings(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let syn::DeriveInput {
        attrs, ident, data, ..
    } = parse_macro_input!(input as syn::DeriveInput);

    let fields: &syn::Fields = match data {
        syn::Data::Struct(ref data) => &data.fields,
        _ => {
            return proc_macro::TokenStream::from(
                syn::Error::new(ident.span(), "Applicable only to struct").to_compile_error(),
            );
        }
    };

    let struct_ident = &ident;

    let index_config_implementation = get_index_config_implementation(struct_ident, fields, attrs);
    proc_macro::TokenStream::from(quote! {
        #index_config_implementation
    })
}

fn filter_attrs(attrs: &[Attribute]) -> impl Iterator<Item = &Attribute> {
    attrs
        .iter()
        .filter(|attr| attr.path().is_ident("index_config"))
}

fn get_index_config_implementation(
    struct_ident: &Ident,
    fields: &syn::Fields,
    attrs: Vec<Attribute>,
) -> proc_macro2::TokenStream {
    let mut primary_key_attribute = String::new();
    let mut distinct_key_attribute = String::new();
    let mut displayed_attributes = vec![];
    let mut searchable_attributes = vec![];
    let mut filterable_attributes = vec![];
    let mut sortable_attributes = vec![];

    let mut index_name_override = None;

    let mut max_total_hits = None;

    let struct_attrs =
        filter_attrs(&attrs).filter_map(|attr| attr.parse_args::<StructAttrs>().ok());
    for struct_attr in struct_attrs {
        if let Some(index_name_value) = struct_attr.index_name {
            index_name_override = Some((index_name_value.value.value(), index_name_value.name_span))
        }

        if let Some(max_total_hits_value) = struct_attr.max_total_hits {
            max_total_hits = Some(max_total_hits_value.value)
        }
    }

    let (index_name, span) = index_name_override.unwrap_or_else(|| {
        (
            struct_ident.to_string().to_case(Case::Snake),
            struct_ident.span(),
        )
    });

    if !is_valid_name(&index_name) {
        return syn::Error::new(span, "Index must follow the naming guidelines.")
            .to_compile_error();
    }

    let mut primary_key_found = false;
    let mut distinct_found = false;

    for field in fields {
        let attrs = filter_attrs(&field.attrs)
            .find_map(|attr| attr.parse_args::<FieldAttrs>().ok())
            .unwrap_or_default();

        // Check if the primary key field is unique
        if attrs.primary_key.value() {
            if primary_key_found {
                return syn::Error::new(
                    field.span(),
                    "Only one field can be marked as primary key",
                )
                .to_compile_error();
            }
            primary_key_attribute = field.ident.clone().unwrap().to_string();
            primary_key_found = true;
        }

        // Check if the distinct field is unique
        if attrs.distinct.value() {
            if distinct_found {
                return syn::Error::new(field.span(), "Only one field can be marked as distinct")
                    .to_compile_error();
            }
            distinct_key_attribute = field.ident.clone().unwrap().to_string();
            distinct_found = true;
        }

        if attrs.displayed.value() {
            displayed_attributes.push(field.ident.clone().unwrap().to_string());
        }

        if attrs.searchable.value() {
            searchable_attributes.push(field.ident.clone().unwrap().to_string());
        }

        if attrs.filterable.value() {
            filterable_attributes.push(field.ident.clone().unwrap().to_string());
        }

        if attrs.sortable.value() {
            sortable_attributes.push(field.ident.clone().unwrap().to_string());
        }
    }

    let primary_key_token: proc_macro2::TokenStream = if primary_key_attribute.is_empty() {
        quote! {
            ::std::option::Option::None
        }
    } else {
        quote! {
            ::std::option::Option::Some(#primary_key_attribute)
        }
    };

    let display_attr_tokens =
        get_settings_token_for_list(&displayed_attributes, "with_displayed_attributes");
    let sortable_attr_tokens =
        get_settings_token_for_list(&sortable_attributes, "with_sortable_attributes");
    let filterable_attr_tokens =
        get_settings_token_for_list(&filterable_attributes, "with_filterable_attributes");
    let searchable_attr_tokens =
        get_settings_token_for_list(&searchable_attributes, "with_searchable_attributes");
    let distinct_attr_token = get_settings_token_for_string_for_some_string(
        &distinct_key_attribute,
        "with_distinct_attribute",
    );

    let pagination_token = get_pagination_token(&max_total_hits, "with_pagination");

    quote! {
        #[::meilisearch_sdk::macro_helper::async_trait(?Send)]
        impl ::meilisearch_sdk::documents::IndexConfig for #struct_ident {
            const INDEX_STR: &'static str = #index_name;

            fn generate_settings() -> ::meilisearch_sdk::settings::Settings {
                ::meilisearch_sdk::settings::Settings::new()
                #display_attr_tokens
                #sortable_attr_tokens
                #filterable_attr_tokens
                #searchable_attr_tokens
                #distinct_attr_token
                #pagination_token
            }

            async fn generate_index<Http: ::meilisearch_sdk::request::HttpClient>(client: &::meilisearch_sdk::client::Client<Http>) -> std::result::Result<::meilisearch_sdk::indexes::Index<Http>, ::meilisearch_sdk::tasks::Task> {
                client.create_index(#index_name, #primary_key_token)
                    .await.unwrap()
                    .wait_for_completion(client, ::std::option::Option::None, ::std::option::Option::None)
                    .await.unwrap()
                    .try_make_index(client)
            }
        }
    }
}

fn get_pagination_token(
    max_hits: &Option<syn::Expr>,
    method_name: &str,
) -> proc_macro2::TokenStream {
    let method_ident = Ident::new(method_name, proc_macro2::Span::call_site());

    match max_hits {
        Some(value) => {
            quote! { .#method_ident(::meilisearch_sdk::settings::PaginationSetting { max_total_hits: #value }) }
        }
        None => quote! {},
    }
}

fn get_settings_token_for_list(
    field_name_list: &[String],
    method_name: &str,
) -> proc_macro2::TokenStream {
    let string_attributes = field_name_list.iter().map(|attr| {
        quote! {
            #attr
        }
    });
    let method_ident = Ident::new(method_name, proc_macro2::Span::call_site());

    if field_name_list.is_empty() {
        quote! {
            .#method_ident(::std::iter::empty::<&str>())
        }
    } else {
        quote! {
            .#method_ident([#(#string_attributes),*])
        }
    }
}

fn get_settings_token_for_string_for_some_string(
    field_name: &String,
    method_name: &str,
) -> proc_macro2::TokenStream {
    let method_ident = Ident::new(method_name, proc_macro2::Span::call_site());

    if field_name.is_empty() {
        proc_macro2::TokenStream::new()
    } else {
        quote! {
            .#method_ident(::std::option::Option::Some(#field_name))
        }
    }
}
