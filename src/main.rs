//! NPLAY: terminal music, radio and podcast player built around mpv.
// Development of the Rust port: the modules are filled in phases.
#![allow(dead_code)]

#[macro_use]
pub mod log;
pub mod app;
pub mod artwork;
pub mod artwork_cache;
pub mod config;
pub mod core;
pub mod db;
pub mod model;
pub mod mpris;
pub mod navigation;
pub mod notifications;
pub mod paths;
pub mod player;
pub mod privacy;
pub mod providers;
pub mod redact;
pub mod service;
pub mod services;
pub mod spotify;
pub mod statistics;
pub mod theme;
pub mod tui;
pub mod visualizer;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version") {
        println!("NPLAY {VERSION}");
        return;
    }
    eprintln!("NPLAY {VERSION}: under construction");
    std::process::exit(2);
}
