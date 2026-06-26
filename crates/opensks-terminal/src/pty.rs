use portable_pty::PtySize;

pub const MIN_COLS: u16 = 20;
pub const MAX_COLS: u16 = 500;
pub const MIN_ROWS: u16 = 5;
pub const MAX_ROWS: u16 = 200;

pub fn normalize_cols(cols: u16) -> u16 {
    cols.clamp(MIN_COLS, MAX_COLS)
}

pub fn normalize_rows(rows: u16) -> u16 {
    rows.clamp(MIN_ROWS, MAX_ROWS)
}

pub fn pty_size(cols: u16, rows: u16) -> PtySize {
    PtySize {
        rows: normalize_rows(rows),
        cols: normalize_cols(cols),
        pixel_width: 0,
        pixel_height: 0,
    }
}
