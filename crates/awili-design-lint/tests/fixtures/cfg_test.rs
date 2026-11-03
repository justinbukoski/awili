// A #[cfg(test)] module body is skipped; the live code around it is scanned.

pub fn live_before() -> Rgba {
    Rgba { r: 0.1, g: 0.1, b: 0.1, a: 1.0 }
}

#[cfg(test)]
mod tests {
    #[test]
    fn must_not_be_scanned() {
        let _a = gpui::rgb(0x112233);
        let _b = gpui::rgba(0x223344);
        let _c = gpui::hsla(0.1, 0.2, 0.3, 0.4);
        let _d = px(12.);
        let _e = theme::z(8.);
        let _f = TEXT_SIZE - 1.0;
        let _g = "#ff0000";
        let _h = Rgba { r: 0.2, g: 0.2, b: 0.2, a: 1.0 };
        let _i = el.opacity(0.55);
    }
}

pub fn live_after() -> Rgba {
    gpui::rgb(0x556677)
}
