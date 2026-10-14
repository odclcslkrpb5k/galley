//! `load` time grows linearly with input (M1 performance review): inputs that once took
//! quadratic time (unclosed HTML tags across blocks, stray closing tags, unclosed inline tags)
//! at sizes n and 4n. Linear growth is about 4×, quadratic 16×; the bound is 8×, wide enough
//! that machine noise does not fail it and narrow enough that quadratic growth always does.

#[cfg(test)]
mod scaling {
    use std::time::{Duration, Instant};

    use galley_core::load;

    /// The fastest of five loads, to take noise out.
    fn best_of_five(input: &str) -> Duration {
        (0..5)
            .map(|_| {
                let started = Instant::now();
                std::hint::black_box(load(input.as_bytes()));
                started.elapsed()
            })
            .min()
            .unwrap_or_default()
    }

    fn assert_linear(name: &str, generate: impl Fn(usize) -> String) {
        let small = best_of_five(&generate(5_000));
        let large = best_of_five(&generate(20_000));
        let ratio = large.as_secs_f64() / small.as_secs_f64().max(1e-6);
        assert!(
            ratio < 8.0,
            "{name}: 4× the input took {ratio:.1}× the time ({small:?} → {large:?})"
        );
    }

    #[test]
    fn unclosed_block_tags_across_paragraphs() {
        assert_linear("unclosed divs", |n| {
            "<div>\n\n".repeat(n) + &"para\n\n".repeat(n)
        });
    }

    #[test]
    fn stray_closing_tags_in_one_paragraph() {
        assert_linear("stray closers", |n| {
            "<b>a ".repeat(n / 2) + &"</i>a ".repeat(n * 2)
        });
    }

    #[test]
    fn unclosed_tags_before_a_long_list() {
        assert_linear("unclosed divs then list", |n| {
            "<div>\n\n".repeat(n / 2) + &"- item\n".repeat(n * 2)
        });
    }
}
