use crate::geom::{CellIdx, LV95};

/// A regular-grid digital elevation model. Cells are square. NaN = nodata.
#[derive(Debug, Clone)]
pub struct Dem {
    pub rows: u32,
    pub cols: u32,
    pub cell_size_m: f32,
    /// LV95 of cell (0,0)'s center.
    pub origin: LV95,
    /// Row-major altitudes in meters MSL.
    pub data: Vec<f32>,
}

impl Dem {
    pub fn new(rows: u32, cols: u32, cell_size_m: f32, origin: LV95) -> Self {
        Self {
            rows,
            cols,
            cell_size_m,
            origin,
            data: vec![f32::NAN; (rows * cols) as usize],
        }
    }

    pub fn from_fn<F: FnMut(u32, u32) -> f32>(
        rows: u32,
        cols: u32,
        cell_size_m: f32,
        origin: LV95,
        mut f: F,
    ) -> Self {
        let mut data = Vec::with_capacity((rows * cols) as usize);
        for r in 0..rows {
            for c in 0..cols {
                data.push(f(r, c));
            }
        }
        Self {
            rows,
            cols,
            cell_size_m,
            origin,
            data,
        }
    }

    #[inline]
    pub fn idx(&self, row: u32, col: u32) -> usize {
        (row * self.cols + col) as usize
    }

    #[inline]
    pub fn get(&self, row: u32, col: u32) -> Option<f32> {
        if row >= self.rows || col >= self.cols {
            return None;
        }
        let v = self.data[self.idx(row, col)];
        if v.is_finite() {
            Some(v)
        } else {
            None
        }
    }

    /// LV95 coordinate of the center of cell (row, col).
    pub fn lv95(&self, row: u32, col: u32) -> LV95 {
        // Convention: row 0 is northernmost, so row index increases as N decreases.
        LV95 {
            e: self.origin.e + col as f64 * self.cell_size_m as f64,
            n: self.origin.n - row as f64 * self.cell_size_m as f64,
        }
    }

    pub fn in_bounds(&self, row: i32, col: i32) -> bool {
        row >= 0 && col >= 0 && row < self.rows as i32 && col < self.cols as i32
    }

    pub fn cell_count(&self) -> usize {
        (self.rows * self.cols) as usize
    }

    pub fn area_m2(&self) -> f64 {
        self.cell_size_m as f64 * self.cell_size_m as f64
    }

    /// Linear distance in meters between two cell centers.
    pub fn distance_m(&self, a: CellIdx, b: CellIdx) -> f32 {
        let dr = a.row as f32 - b.row as f32;
        let dc = a.col as f32 - b.col as f32;
        (dr * dr + dc * dc).sqrt() * self.cell_size_m
    }
}
