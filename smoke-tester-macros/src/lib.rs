use proc_macro_error2::{abort, proc_macro_error};
use proc_macro2::Span;
use quote::quote;
use syn::{
    Ident, ItemFn, Token, parse::Parse, parse_macro_input, punctuated::Punctuated, spanned::Spanned,
};

#[derive(Clone, Copy)]
enum TestKind {
    /// Tests which take a `probe_rs::Session` as an argument
    Session { _kw: _kw::session },
    /// Tests which take a `probe_rs::Core` as an argument
    Core { _kw: _kw::core },
}

mod _kw {
    syn::custom_keyword!(core);
    syn::custom_keyword!(session);
}

impl Parse for TestKind {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let lookahead = input.lookahead1();

        if lookahead.peek(_kw::core) {
            let kw: _kw::core = input.parse()?;
            Ok(Self::Core { _kw: kw })
        } else if lookahead.peek(_kw::session) {
            let kw: _kw::session = input.parse()?;
            Ok(Self::Session { _kw: kw })
        } else {
            Err(lookahead.error())
        }
    }
}

struct TestInfo {
    kind: TestKind,
    test_fn: ItemFn,
}

struct Args {
    kind: TestKind,
}

impl Parse for Args {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let parts = Punctuated::<TestKind, Token![,]>::parse_terminated(input)?;

        let Some(kind) = parts.first() else {
            abort!(input.span(), "Should have exactly one attribute")
        };

        Ok(Args { kind: *kind })
    }
}

#[proc_macro_attribute]
#[proc_macro_error]
pub fn test(
    args: proc_macro::TokenStream,
    item: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    let item: proc_macro2::TokenStream = parse_macro_input!(item);
    let args = parse_macro_input!(args as Args);
    let test_info = parse(args, item);

    match test_info.kind {
        TestKind::Session { _kw: _ } => session_test(test_info),
        TestKind::Core { _kw: _ } => core_test(test_info),
    }
}

fn session_test(test_info: TestInfo) -> proc_macro::TokenStream {
    let test_fn = &test_info.test_fn;

    let struct_name = format!("TEST_{}", test_fn.sig.ident);

    let ident = Ident::new(&struct_name, Span::call_site());
    let name = test_fn.sig.ident.to_string();

    let test_fn_pointer = test_fn.sig.ident.clone();

    let with_attr = quote! {
        #test_fn

        #[distributed_slice(SESSION_TESTS)]
        static #ident: crate::NamedSessionTest = crate::NamedSessionTest::new(#name, &(#test_fn_pointer as _));

    };

    with_attr.into()
}

fn core_test(test_info: TestInfo) -> proc_macro::TokenStream {
    let test_fn = &test_info.test_fn;

    let struct_name = format!("TEST_{}", test_fn.sig.ident);

    let ident = Ident::new(&struct_name, Span::call_site());
    let name = test_fn.sig.ident.to_string();

    let test_fn_pointer = test_fn.sig.ident.clone();

    let with_attr = quote! {
        #test_fn

        #[distributed_slice(CORE_TESTS)]
        static #ident: crate::NamedCoreTest = crate::NamedCoreTest::new(#name, &(#test_fn_pointer as _));

    };

    with_attr.into()
}

fn parse(args: Args, item: proc_macro2::TokenStream) -> TestInfo {
    let item_span = item.span();

    let test_fn = match syn::parse2(item) {
        Ok(parsed) => parsed,
        Err(_err) => abort!(item_span, "Can only be used on functions"),
    };

    TestInfo {
        kind: args.kind,
        test_fn,
    }
}
