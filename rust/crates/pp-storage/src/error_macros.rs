// Shared formatting for errors whose Display contract is their Debug text.
#[doc(hidden)]
#[macro_export]
macro_rules! impl_debug_error {
    ($error:ty) => {
        impl ::std::fmt::Display for $error {
            fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                write!(formatter, "{self:?}")
            }
        }
        impl ::std::error::Error for $error {}
    };
}
