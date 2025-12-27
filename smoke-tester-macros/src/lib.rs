use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::quote;
use syn::{Ident, ItemFn};

#[proc_macro_attribute]
pub fn session_test(_args: TokenStream, item: TokenStream) -> TokenStream {
    let test_fn: ItemFn = syn::parse(item).unwrap();

    let struct_name = format!("TEST_{}", test_fn.sig.ident);

    let ident = Ident::new(&struct_name, Span::call_site());
    let name = test_fn.sig.ident.to_string();

    let test_fn_pointer = test_fn.sig.ident.clone();

    let with_attr = quote! {
        #test_fn

        #[distributed_slice(SESSION_TESTS)]
        static #ident: crate::NamedSessionTest = crate::NamedSessionTest::new(#name, &(#test_fn_pointer as _));

    };

    proc_macro2::TokenStream::from(with_attr).into()
}

#[proc_macro_attribute]
pub fn core_test(_args: TokenStream, item: TokenStream) -> TokenStream {
    let test_fn: ItemFn = syn::parse(item).unwrap();

    let struct_name = format!("TEST_{}", test_fn.sig.ident);

    let ident = Ident::new(&struct_name, Span::call_site());
    let name = test_fn.sig.ident.to_string();

    let test_fn_pointer = test_fn.sig.ident.clone();

    let with_attr = quote! {
        #test_fn

        #[distributed_slice(CORE_TESTS)]
        static #ident: crate::NamedCoreTest = crate::NamedCoreTest::new(#name, &(#test_fn_pointer as _));

    };

    proc_macro2::TokenStream::from(with_attr).into()
}
