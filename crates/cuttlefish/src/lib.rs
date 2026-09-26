//! Cuttlefish: an AI reviewer for Splatoon 3 Salmon Run gameplay, with a
//! knowledge store far larger than a model's context.
//!
//! Sources are imported as [`doc::Document`]s (web pages and wikis through
//! [`crawl`] and [`html`], YouTube transcripts through [`youtube`], Discord
//! conversations through [`discord`], local files through [`mod@file`]), split
//! into chunks ([`chunk`]), embedded ([`embed`]) and indexed ([`index`]) in a
//! data folder ([`store`]); [`ingest`] runs the importers. A [`glossary`] maps jargon across languages.
//! [`review::Reviewer`] retrieves what is relevant to a moment or question
//! and asks the model through the Anthropic Messages API ([`llm`]).
//!
//! See the crate README for the design and the `cuttlefish` CLI.

extern crate alloc;

pub mod chunk;
pub mod crawl;
pub mod discord;
pub mod doc;
pub mod embed;
pub mod eval;
pub mod file;
pub mod glossary;
pub mod html;
pub mod index;
pub mod ingest;
pub mod llm;
pub mod review;
pub mod store;
pub mod youtube;
