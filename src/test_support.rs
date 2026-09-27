pub(crate) fn panic_after_draw_requested() -> bool {
    std::env::var_os("TERRALEPH_TEST_PANIC_AFTER_DRAW").is_some()
}
