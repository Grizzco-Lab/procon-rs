//! Cuttlefish: an AI reviewer for Splatoon 3 Salmon Run gameplay, with a
//! knowledge store far larger than a model's context.
//!
//! Sources are imported as [`doc::Document`]s (web pages through [`crawl`]
//! and [`html`], whole wiki topics and sites through [`wiki`], Google Docs,
//! Sheets and Slides through
//! [`google`], YouTube transcripts through [`youtube`], Discord
//! conversations through [`discord`], local files through [`mod@file`]), split
//! into chunks ([`chunk`]), embedded ([`embed`]) and indexed ([`index`]) in a
//! data folder ([`store`]); [`ingest`] runs the importers. A [`glossary`] maps jargon across
//! languages: official names and the slang players use, which the user teaches and approves
//! ([`slang`], with suggestions the model finds in the store). Every source
//! carries the game era it is about ([`game`]).
//! The [`inbox`] takes anything dropped into the data folder: prose becomes
//! documents, multilingual name tables become glossary terms ([`tables`];
//! message folders such as stat.ink's PHP ones through [`messages`] and
//! [`php`]), images an asset catalogue ([`assets`]).
//! The #vod-review archive becomes a corpus of reviewed VODs ([`corpus`]),
//! their videos downloaded at 480p ([`corpus_videos`]) and each conversation
//! a review of the studio ([`corpus_reviews`]); each reviewer's comment is
//! a unit of retrieval of its own ([`expert`]), found for a moment through
//! a text summary of it ([`situation`]: controller input, HUD, objects).
//! [`review::Reviewer`] retrieves what is relevant to a moment or question
//! and asks the model through the Anthropic Messages API or the Claude Code
//! CLI ([`llm`], [`claude_cli`]). The player's own corrections are expert
//! notes ([`notes`]), the most trusted source; a bank of deep questions
//! ([`questions`]) is asked through [`deep_eval`] and the answers reviewed
//! into notes.
//!
//! See the crate README for the design and the `cuttlefish` CLI.

extern crate alloc;

pub mod assets;
pub mod chunk;
pub mod claude_cli;
pub mod corpus;
pub mod corpus_reviews;
pub mod corpus_videos;
pub mod crawl;
pub mod deep_eval;
pub mod discord;
pub mod discord_fetch;
pub mod discord_media;
pub mod doc;
pub mod embed;
pub mod env_file;
pub mod eval;
pub mod expert;
pub mod file;
pub mod game;
pub mod glossary;
pub mod google;
pub mod html;
pub mod inbox;
pub mod index;
pub mod ingest;
pub mod llm;
pub mod lock;
pub mod messages;
pub mod moments;
pub mod notes;
pub mod pedia;
pub mod php;
pub mod questions;
pub mod review;
pub mod situation;
pub mod slang;
pub mod store;
pub mod tables;
pub mod wiki;
pub mod youtube;
