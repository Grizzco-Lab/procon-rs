# Cuttlefish

Knowledge store and model backend for Cuttlefish, the studio's AI reviewer
for Splatoon 3 Salmon Run. It imports guides, wikis, video transcripts and
Discord VOD-review discussions into a local store; retrieves what matters for
a moment of gameplay or a question; and asks Claude (Anthropic Messages API)
with the frames, the retrieved knowledge and a jargon glossary. Library
(`cuttlefish`) plus a CLI of the same name.

## Quick start

```bash
cargo build --release -p cuttlefish
alias cuttlefish=target/release/cuttlefish

# Data lives outside the repo: --data, $CUTTLEFISH_DATA or ~/.local/share/cuttlefish
export CUTTLEFISH_DATA=~/cuttlefish-data
export CUTTLEFISH_CONTACT=you@example.org   # put in the crawler's User-Agent

cuttlefish ingest file fundamentals.pdf --source guide --license "by permission of the authors"
cuttlefish ingest url https://example.org/guide --source guide
cuttlefish ingest url --sitemap https://example.org/sitemap.xml --max-pages 100
cuttlefish ingest url --mediawiki https://wiki.example.org/w/api.php --category "Category:Salmon Run"
cuttlefish ingest youtube https://www.youtube.com/playlist?list=...
cuttlefish ingest discord-export vod-review.json
DISCORD_BOT_TOKEN=... cuttlefish ingest discord-bot --channel 123456789012345678

cuttlefish search "バクダンの処理"          # top-k chunks with sources; any language
cuttlefish glossary "Steelhead"            # a term's names and definition
cuttlefish stats
cuttlefish eval eval.example.toml          # retrieval check, no key needed

export ANTHROPIC_API_KEY=...               # only ever from the environment
cuttlefish ask "When should I leave the basket to kill a Stinger?"
cuttlefish translate "Kill the Steelhead before the Flyfish" --to ja
cuttlefish eval eval.example.toml --answer
```

The first command that embeds downloads the embedding model (about 470 MB)
into `<data>/models/`. `--model` / `$CUTTLEFISH_MODEL` picks the model
(default `claude-opus-5-5`), `--effort` its effort (default `high`).
`RUST_LOG=debug` shows more.

## Data folder

```text
<data>/
  glossary.toml      your glossary; the crate's glossary.toml seed until you add one
  digest.md          curated fundamentals, sent with every request (optional)
  raw/<kind>/        pages, subtitles, exports as received
  docs/<id>.json     processed documents with source, url, title, language,
                     license, attribution, fetch time, weight and text
  index/             meta.json, entries.jsonl, vectors.f32
  models/            embedding model files
```

Ingesting the same url again replaces its document (`--refresh` refetches
pages already stored). `cuttlefish reindex` re-embeds everything after a
change of embedder or chunk sizes.

## Sources and their terms

| Source | How | Notes |
|---|---|---|
| Guides ("Overfishing Fundamentals", Lenny, ...) | `ingest file` (md, txt, html, pdf) or `ingest url` | `--source guide` (weight 1.15); record the license with `--license` |
| Inkipedia, other MediaWiki wikis | `ingest url --mediawiki <api.php> --category ...` | Article content through the API; the site's license is read from `siteinfo` (Inkipedia: CC BY-NC-SA) and kept per document. See the note below |
| Other sites, stat.ink docs | `ingest url` (urls, `--list`, `--sitemap`) | robots.txt obeyed, one request per site every 3 s or the site's `Crawl-delay` |
| YouTube | `ingest youtube <video/playlist/channel>` | `yt-dlp` fetches subtitles and metadata only; uploaded subtitles preferred over auto captions |
| Discord #vod-review | `ingest discord-export` or `ingest discord-bot` | Never with a user token (against Discord's terms). See below |
| Twitter/X, Twitch | not automated | X's API terms and pricing rule out scraping; save the posts or threads you value as text and `ingest file`. Twitch VODs have no subtitles (a speech-to-text step would be needed) |

**Discord.** Two supported ways, both needing the server's consent:
1. *Export file*: someone with access runs DiscordChatExporter (JSON format)
   on the channel or its threads and gives you the file. `--whole` keeps a
   file as one conversation (a thread or forum post).
2. *Bot*: a server admin creates a bot in the Discord developer portal,
   enables the Message Content intent, invites it with View Channel and Read
   Message History on the channel, and shares its token; pass it as
   `DISCORD_BOT_TOKEN` (never stored). Threads and forum posts are read
   too unless `--no-threads`.

Channel messages are grouped into conversations (split at 2-hour gaps);
threads stay whole. Clips stay as links; they are not downloaded. A channel
named `vod-review` (or its threads) gets the highest weight (1.2).

**Inkipedia.** Its `robots.txt` allows general crawlers (`*`) on articles and
`api.php`, but disallows AI crawlers such as ClaudeBot and GPTBot entirely.
Cuttlefish identifies as itself and is run by you for personal study, so the
rules for `*` apply to it, but feeding the wiki to a model is close to what
those lines refuse. Worth asking the Inkipedia admins, or using a database
dump they publish, before a large import; CC BY-NC-SA also means
non-commercial use with attribution, and derived text shared under the
same license.

## Library API (for the studio)

```rust
use cuttlefish::review::{Reviewer, ReviewRequest, Frame, ExistingComment};
use cuttlefish::llm::Settings;

let reviewer = Reviewer::open(&data_dir, Settings::default())?; // fails clearly without ANTHROPIC_API_KEY
let comments = reviewer.review(&ReviewRequest {
    video: "2026-09-25_20-15-00".into(),
    start_s: 120.0,
    end_s: 135.0,
    frames: vec![Frame { t_s: 121.0, jpeg }],       // up to 20 are sent
    question: Some("Why did we lose the basket here?".into()),
    comments: vec![ExistingComment { t_s: 124.0, text: "I died here".into(), author: None }],
})?;
let answer = reviewer.ask("...")?;
let ja = reviewer.translate("...", "ja")?;
```

`review` blocks (seconds to a minute); call it from a blocking task.
`AiComment` serializes as:

```json
{"t_s": 124.5, "t_end_s": 128.0, "text": "...",
 "shapes": [{"kind": "arrow", "x0": 0.2, "y0": 0.7, "x1": 0.5, "y1": 0.5, "label": "basket"}],
 "sources": [{"id": "S2", "title": "...", "heading": "...", "url": "...",
              "source": "discord-vod-review", "license": "..."}]}
```

Shapes are in 0-1 frame coordinates (x right, y down); a box goes from its
top-left to its bottom-right corner, an arrow from tail to head.

## Design: knowledge far larger than the context

The sources add up to far more text than fits in one request, and most of it
is irrelevant to any one moment. So the model gets a small, stable core
every time and the relevant slice of the rest on demand.

**1. Retrieval, per request.** Every document is split into chunks of about
400 tokens (by headings, then paragraphs, then sentences, with about 60
tokens of overlap when a section is cut) and embedded with
`intfloat/multilingual-e5-small`. A review retrieves the 8 chunks nearest to
the player's question plus the comments already on the moment (without a
question, a "fundamentals" query). Asking: nearest to the question.
Retrieval runs *before* the call, not as a tool the model calls: one
request, predictable cost and latency. A search tool for the model is the
next step if single-shot retrieval misses too often (the model could then
query "Stinger at low tide on Sockeye Station" once it recognizes the
stage).

**2. The core: curated digest.** `<data>/digest.md` is a hand-written (or
model-drafted, then human-checked) summary of the fundamentals: the
priorities, egg-flow rules and per-stage/per-tide plans that the best
sources agree on, a few thousand tokens. It sits in the system prompt, which
is cached, so it costs a tenth of normal input after the first call. Keep it
short and opinionated; retrieval covers the long tail.

**3. Glossary for jargon.** `glossary.toml` lists terms with names per
language and a definition. Terms found in a query add their other-language
names to the query (a Japanese question finds English notes; the embedder
is multilingual too), and the matched terms go into the prompt so answers
and translations use the community's names. The seed has English and
Japanese; add Chinese, Spanish, Russian and French names there.

**4. Source quality.** Each document has a weight: #vod-review 1.2, guides
1.15, wikis/Discord/files 1.0, web pages and video transcripts 0.9
(`--weight` overrides). Ranking adds 0.1 x (weight - 1) to the cosine, which
reorders close matches without burying a clearly better one. The prompt
tells the model that high-level review outweighs generic pages and to cite
only excerpts it was given; every source keeps its license and url so
citations are clickable.

**5. Evaluation.** `eval.example.toml` shows the format: questions with the
sources that should be retrieved and points a good answer makes. Build 30-50
real questions from #vod-review threads (with the answer the experts gave),
run `cuttlefish eval` after every change to sources, chunking, weights or
prompts, and `--answer` before changing models or prompts. Retrieval misses
are the cheapest to find and fix; for answers, read them next to the expected
points (a model-graded rubric is the next step).

**6. When fine-tuning would make sense.** Not for knowledge: facts change
with patches and rotations, and retrieval updates by re-ingesting. It could
pay off later for *style and judgment* (reviews that sound like the best
reviewers, calibrated boss priority), once there are a few thousand accepted
review comments from the studio to learn from, or to distil a cheap model
for high-volume work. Until then prompt + digest + retrieval is cheaper to
iterate on.

**7. Cost per review.** Claude Opus 5.5 is $4 per million input tokens and
$20 per million output tokens (thinking counts as output). An image
costs about width x height / 750 tokens: 1280x720 is about 1,230 tokens, 640x360
about 310. A review with 20 frames at 720p plus 8 excerpts (about 4k tokens)
is about 29k input tokens ($0.12) and 2-4k output tokens including thinking
($0.04-0.08): **about $0.15-0.20**. With 10 frames at 640x360: about 7k input,
**about $0.07**. The cached system prompt and digest (cache reads at $0.20
per million) are negligible. Fewer, smaller frames and `--effort medium` are
the levers; the Batch API halves prices for offline work such as drafting
digests.

**8. Scaling the store.** The flat index scans every vector: 100k chunks x
384 floats is 150 MB and searches in tens of milliseconds, enough for
every source listed here. Beyond that, implement `index::VectorIndex` with an
approximate index (HNSW) or a vector database; documents, chunks and
metadata stay as they are. Other upgrades behind the same interfaces: a
larger embedder (BGE-M3, 568M parameters, 8k-token inputs, also XLM-RoBERTa
based) with `reindex`; hybrid keyword + vector search for exact names and
numbers; re-ranking the top 30 with a cross-encoder; CUDA embeddings with
`--features cuda`.

### Embedding model

`intfloat/multilingual-e5-small` (MIT license): 118M parameters, 384
dimensions, trained on about 100 languages including Japanese and Chinese,
a standard BERT architecture that candle runs directly, and fast on a CPU
(about 470 MB of weights). It reads at most 512 tokens, which is why chunks
stay near 400. The revision is pinned so vectors never mix model versions.
BGE-M3 (MIT) retrieves better on long and mixed-language text but is five
times larger; it is the natural upgrade once a GPU is available.

### Model calls

`llm.rs` posts to `https://api.anthropic.com/v1/messages` with ureq: the
system prompt as a cached block, one user turn (knowledge excerpts,
glossary, existing comments, frames as base64 JPEG, the task), adaptive
thinking (always on for Claude Opus 5.5) with an explicit effort, and for
reviews a JSON schema (structured output) for the comments. Refusals and
truncated answers are reported as errors; 429 and 5xx are retried twice.
Excerpts are marked as reference material, not instructions, since they
come from the web and chat. Unit tests never touch the network: requests
are built and answers parsed by pure functions, and a fake transport stands
in for HTTPS.
