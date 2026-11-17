//! T3 Agents without a presenter.
//!
//! Model, read-only T3 projection, synthetic demo and the cell-grid composition of the
//! dashboard. Front doors (the Rune window, a terminal presenter, status output) depend on
//! this crate; it depends on no drawing or windowing library.
pub mod bench;
pub mod probe;
pub mod cli;
pub mod frame_bench;
pub mod clock;
pub mod cluster;
pub mod demo;
pub mod details;
pub mod host;
pub mod integration;
pub mod model;
pub mod overview;
pub mod platform;
pub mod product;
pub mod source;
pub mod status;
pub mod terminal;
pub mod ui;

pub mod live;
