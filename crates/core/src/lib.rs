//! T3 Agents without a presenter.
//!
//! Model, read-only T3 projection, synthetic demo and the cell-grid composition of the
//! dashboard. Front doors (the Rune window, a terminal presenter, status output) depend on
//! this crate; it depends on no drawing or windowing library.
pub mod anatomy;
pub mod asks;
pub mod bench;
#[cfg(unix)]
pub(crate) mod cache;
pub mod cache_path;
pub mod probe;
pub mod cli;
pub mod frame_bench;
pub mod clock;
pub mod cluster;
pub mod demo;
pub mod details;
pub mod fanout;
pub mod host;
pub mod headroom;
pub mod integration;
pub mod model;
pub mod overview;
pub mod platform;
pub mod product;
pub mod program;
pub mod recap;
pub mod recap_view;
pub mod skim;
pub mod source;
pub mod status;
pub mod storage;
pub mod storage_view;
pub mod terminal;
pub mod theme;
pub mod tools;
pub mod tools_view;
pub mod ui;

pub mod live;

pub mod trace;
pub mod trace_live;
pub mod trace_view;
