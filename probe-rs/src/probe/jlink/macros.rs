macro_rules! enum_and_set {
    (
        $( #[$enum_attr:meta] )*
        $enum_vis:vis enum $enum_name:ident {
            $(
                $( #[$attr:meta] )*
                $name:ident = $id:expr,
            )+
        }

        flags $flags_name:ident: $flags_ty:ty;
    ) => {
        $( #[$enum_attr] )*
        $enum_vis enum $enum_name {
            $(
                #[allow(missing_docs)] // TODO the capabilities should be documented
                $( #[$attr] )*
                $name = $id,
            )+
        }

        impl $enum_name {
            #[allow(dead_code)]
            const ALL: &'static [Self] = &[
                $( Self::$name ),+
            ];
        }

        ::bitflags::bitflags! {
            #[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
            struct $flags_name: $flags_ty {
                $(
                    const $name = 1 << $id;
                )+
            }
        }
    };
}
