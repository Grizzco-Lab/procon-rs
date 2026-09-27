//! Cuttlefish: an AI reviewer for Splatoon 3 Salmon Run gameplay, with a
//! knowledge store far larger than a model's context.
//!
//! Sources are imported as [`doc::Document`]s (web pages and wikis through
//! [`crawl`] and [`html`], Google Docs, Sheets and Slides through
//! [`google`], YouTube transcripts through [`youtube`], Discord
//! conversations through [`discord`], local files through [`mod@file`]), split
//! into chunks ([`chunk`]), embedded ([`embed`]) and indexed ([`index`]) in a
//! data folder ([`store`]); [`ingest`] runs the importers. A [`glossary`] maps jargon across languages.
//! The [`inbox`] takes anything dropped into the data folder: prose becomes
//! documents, multilingual name tables become glossary terms ([`tables`];
//! message folders such as stat.ink's PHP ones through [`messages`] and
//! [`php`]), images an asset catalogue ([`assets`]).
//! [`review::Reviewer`] retrieves what is relevant to a moment or question
//! and asks the model through the Anthropic Messages API or the Claude Code
//! CLI ([`llm`], [`claude_cli`]).
//!
//! See the crate README for the design and the `cuttlefish` CLI.

extern crate alloc;

pub mod assets;
pub mod chunk;
pub mod claude_cli;
pub mod crawl;
pub mod discord;
pub mod doc;
pub mod embed;
pub mod eval;
pub mod file;
pub mod glossary;
pub mod google;
pub mod html;
pub mod inbox;
pub mod index;
pub mod ingest;
pub mod llm;
pub mod messages;
pub mod php;
pub mod review;
pub mod store;
pub mod tables;
pub mod youtube;
