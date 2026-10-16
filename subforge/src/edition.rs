//! Image edition is a property of the compiled binary, never deployment state.
//! The CUDA feature enables eligibility checks; runtime grants and health checks
//! still control whether a GPU worker may run.

pub const CUDA_ENABLED: bool = cfg!(feature = "cuda");
pub const NAME: &str = if CUDA_ENABLED { "cuda" } else { "cpu" };

#[cfg(test)]
mod tests {
    #[test]
    fn edition_matches_the_compiled_feature() {
        #[cfg(feature = "cuda")]
        assert_eq!(super::NAME, "cuda");
        #[cfg(not(feature = "cuda"))]
        assert_eq!(super::NAME, "cpu");
        assert_eq!(super::CUDA_ENABLED, cfg!(feature = "cuda"));
    }
}
