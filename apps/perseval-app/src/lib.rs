#![forbid(unsafe_code)]

//! Native GPUI-CE shell for the Perseval trace workbench.

mod app;
mod blocking;
mod components;
pub mod controllers;
pub mod design;
mod icons;
pub mod screens;
pub mod workbench;

pub use app::PersevalApp;

pub const PRODUCT_NAME: &str = "Perseval";
pub const FRONTEND: &str = "GPUI-CE";
