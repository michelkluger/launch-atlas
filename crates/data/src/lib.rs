//! Swisstopo data ingestion: LV95 conversions, SwissALTI3D tile fetch, region build.

pub mod bafu;
pub mod lifts;
pub mod lv95;
pub mod obstacles;
pub mod region;
pub mod stac;
pub mod tiff_reader;
pub mod trails;
pub mod warnings;

pub use lifts::{Lift, LiftEndpoint, LiftKind, fetch_lifts, load_lifts, save_lifts};
pub use lv95::{lv95_to_wgs84, wgs84_to_lv95};
pub use region::{BBox, Region, fetch_multi_region, fetch_region, load_region, save_region};
pub use warnings::{
    Severity, Warning, WarningKind, fetch_warnings, load_warnings, point_in_polygon, save_warnings,
};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum DataError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("no DEM tile in STAC item {0}")]
    MissingAsset(String),
    #[error("invalid region binary: {0}")]
    BadFormat(String),
}

pub type Result<T> = std::result::Result<T, DataError>;
