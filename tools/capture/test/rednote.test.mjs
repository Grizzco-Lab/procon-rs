// The site's answers and page state read into notes, comments, lists and
// accounts; the page markers; the record and its file
import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import * as rn from "../lib/rednote.mjs";

const API = "https://edith.xiaohongshu.com/api/sns/web";
const SR = "66aa00000000000000000001";
const CREATOR = "5f0000000000000000000001";

/** The API's shape of a note detail (`/feed`) */
export function feedFixture() {
  return {
    code: 0,
    success: true,
    msg: "成功",
    data: {
      cursor_score: "",
      items: [
        {
          id: SR,
          model_type: "note",
          note_card: {
            note_id: SR,
            type: "video",
            title: "打工400分教学",
            desc: "第一波决定一切：先处理炸弹鱼。 #打工[话题]#",
            time: 1725000000000,
            last_update_time: 1725003600000,
            ip_location: "上海",
            user: {
              user_id: CREATOR,
              nickname: "Grizzco Coach",
              avatar: "https://x/a.jpg",
            },
            tag_list: [
              { id: "t1", name: "打工", type: "topic" },
              { id: "t2", name: "Splatoon3", type: "topic" },
            ],
            image_list: [
              {
                url_default: "https://sns-img/1.jpg",
                info_list: [
                  { image_scene: "WB_DFT", url: "https://sns-img/1-dft.jpg" },
                ],
              },
              {
                info_list: [
                  { image_scene: "WB_PRV", url: "https://sns-img/2-prv.jpg" },
                ],
              },
            ],
            video: {
              media: {
                stream: { h264: [{ master_url: "https://sns-video/1.mp4" }] },
              },
            },
            interact_info: {
              liked_count: "1.2万",
              collected_count: "3,210",
              comment_count: "88",
              share_count: "12",
            },
            xsec_token: "ABtok==",
          },
        },
      ],
    },
  };
}

/** The API's shape of a comments page */
export function commentsFixture(hasMore) {
  return {
    code: 0,
    success: true,
    data: {
      cursor: "c2",
      has_more: hasMore,
      time: 1725100000000,
      user_id: "me",
      comments: [
        {
          id: "c1",
          note_id: SR,
          content: "Kill the Steelhead before the Flyfish",
          create_time: 1725010000000,
          like_count: "5",
          ip_location: "北京",
          user_info: { user_id: "u2", nickname: "alice", image: "" },
          sub_comment_count: "2",
          sub_comment_has_more: true,
          sub_comment_cursor: "s1",
          sub_comments: [
            {
              id: "c1-1",
              content: "Only when it is at the shore",
              create_time: 1725020000000,
              like_count: 1,
              user_info: { user_id: "u3", nickname: "bob" },
              target_comment: {
                id: "c1",
                user_info: { user_id: "u2", nickname: "alice" },
              },
            },
          ],
        },
        {
          id: "c2",
          content: "nice",
          create_time: 1725030000000,
          like_count: 0,
          user_info: { user_id: "u4", nickname: "carol" },
          sub_comment_count: 0,
          sub_comments: [],
        },
      ],
    },
  };
}

/** The page state's camel-case shape */
export function stateFixture() {
  const id = "66aa00000000000000000002";
  return {
    note: {
      noteDetailMap: {
        [id]: {
          note: {
            noteId: id,
            type: "normal",
            title: "サーモンラン tips",
            desc: "Tower first.",
            time: 1700000000000,
            user: { userId: CREATOR, nickname: "Grizzco Coach" },
            tagList: [{ name: "salmonrun" }],
            imageList: [{ urlDefault: "https://sns-img/2.jpg" }],
            interactInfo: {
              likedCount: "42",
              collectedCount: "7",
              commentCount: "1",
              shareCount: "0",
            },
            xsecToken: "tok2",
          },
          comments: {
            list: [
              {
                id: "k1",
                content: "agree",
                createTime: 1700001000000,
                likeCount: 2,
                userInfo: { userId: "u9", nickname: "dan" },
                subComments: [],
                subCommentCount: 0,
              },
            ],
            hasMore: false,
          },
        },
      },
    },
    user: {
      notes: [
        [
          {
            id,
            noteCard: {
              noteId: id,
              displayTitle: "サーモンラン tips",
              xsecToken: "tok2",
              user: { userId: CREATOR },
            },
          },
          {
            id: "66aa00000000000000000003",
            noteCard: {
              noteId: "66aa00000000000000000003",
              displayTitle: "My cat",
              xsecToken: "tok3",
            },
          },
        ],
      ],
      userInfo: { userId: "me1", nickname: "Me" },
    },
  };
}

test("counts and times as the site writes them", () => {
  assert.equal(rn.count(12), 12);
  assert.equal(rn.count("1,234"), 1234);
  assert.equal(rn.count("1.2万"), 12000);
  assert.equal(rn.count("3亿"), 300_000_000);
  assert.equal(rn.count("999+"), 999);
  assert.equal(rn.count(""), 0);
  assert.equal(rn.count(null), 0);
  assert.equal(rn.timeOf(1725000000000), "2024-08-30T06:40:00.000Z");
  assert.equal(rn.timeOf(1725000000), "2024-08-30T06:40:00.000Z");
  assert.equal(rn.timeOf("1725000000"), "2024-08-30T06:40:00.000Z");
  assert.equal(rn.timeOf(0), null);
  assert.equal(rn.timeOf("soon"), null);
});

test("a feed answer gives the note without its comments", () => {
  const p = rn.recognise(`${API}/v1/feed`, feedFixture());
  assert.equal(p.kind, "notes");
  const n = p.notes[0];
  assert.equal(n.id, SR);
  assert.equal(n.url, `https://www.xiaohongshu.com/explore/${SR}`);
  assert.deepEqual(n.author, { user_id: CREATOR, nickname: "Grizzco Coach" });
  assert.equal(n.kind, "video");
  assert.equal(n.date, "2024-08-30T06:40:00.000Z");
  assert.equal(n.updated, "2024-08-30T07:40:00.000Z");
  assert.deepEqual(n.tags, ["打工", "Splatoon3"]);
  assert.deepEqual(n.images, [
    "https://sns-img/1.jpg",
    "https://sns-img/2-prv.jpg",
  ]);
  assert.equal(n.video, "https://sns-video/1.mp4");
  assert.deepEqual(
    [n.likes, n.collects, n.comment_count, n.shares],
    [12000, 3210, 88, 12],
  );
  assert.equal(n.xsec_token, "ABtok==");
  assert.deepEqual(n.comments, []);
  // A video known only by its key
  const card = feedFixture().data.items[0].note_card;
  card.video = { consumer: { origin_video_key: "pre/abc" } };
  assert.equal(
    rn.noteFrom(card).video,
    "https://sns-video-bd.xhscdn.com/pre/abc",
  );
  assert.equal(rn.noteFrom({ title: "no id" }), null);
});

test("comments come with their replies; a replies page names its root", () => {
  const url = `${API}/v2/comment/page?note_id=${SR}&cursor=&top_comment_id=&image_formats=jpg`;
  const p = rn.recognise(url, commentsFixture(true));
  assert.equal(p.kind, "comments");
  assert.equal(p.note_id, SR);
  assert.equal(p.root, null);
  assert.equal(p.has_more, true);
  assert.equal(p.comments.length, 2);
  const c = p.comments[0];
  assert.equal(c.author.nickname, "alice");
  assert.equal(c.likes, 5);
  assert.equal(c.location, "北京");
  assert.equal(c.replies_total, 2);
  assert.equal(c.date, "2024-08-30T09:26:40.000Z");
  assert.deepEqual(
    [
      c.replies[0].reply_to,
      c.replies[0].reply_to_author,
      c.replies[0].author.nickname,
    ],
    ["c1", "alice", "bob"],
  );
  assert.equal(p.comments[1].location, null);
  const sub = `${API}/v2/comment/sub/page?note_id=${SR}&root_comment_id=c1&num=10&cursor=s1`;
  assert.equal(rn.recognise(sub, commentsFixture(false)).root, "c1");
  assert.equal(rn.commentFrom({ like_count: 3 }), null);
});

test("the page state holds notes with comments and the creator's list", () => {
  const notes = rn.notesFromState(stateFixture());
  assert.equal(notes.length, 1);
  const n = notes[0];
  assert.equal(n.title, "サーモンラン tips");
  assert.equal(n.likes, 42);
  assert.deepEqual(n.images, ["https://sns-img/2.jpg"]);
  assert.equal(n.comments.length, 1);
  assert.equal(n.comments[0].author.nickname, "dan");
  assert.equal(n.comments_complete, true);
  const listed = rn.listedFromState(stateFixture());
  assert.equal(listed.length, 2);
  assert.deepEqual(listed[0], {
    id: "66aa00000000000000000002",
    title: "サーモンラン tips",
    xsec_token: "tok2",
    author_id: CREATOR,
  });
  assert.equal(listed[1].title, "My cat");
  assert.equal(listed[1].author_id, null);
  assert.deepEqual(rn.notesFromState(null), []);
  assert.deepEqual(rn.listedFromState({ user: { notes: "x" } }), []);
  assert.equal(
    rn.noteUrl("66aa00000000000000000002", "tok2"),
    "https://www.xiaohongshu.com/explore/66aa00000000000000000002?xsec_token=tok2&xsec_source=pc_user",
  );
});

test("lists, followings, the account and refusals are recognised", () => {
  const list = {
    code: 0,
    success: true,
    data: {
      cursor: "x",
      has_more: true,
      notes: [
        {
          note_id: "66aa00000000000000000004",
          type: "normal",
          display_title: "鲑鱼跑 W3",
          xsec_token: "t4",
          user: { user_id: CREATOR, nickname: "Grizzco Coach" },
        },
        { note_id: "bad" },
      ],
    },
  };
  const l = rn.recognise(
    `${API}/v1/user_posted?num=30&cursor=&user_id=${CREATOR}`,
    list,
  );
  assert.equal(l.kind, "list");
  assert.equal(l.has_more, true);
  assert.deepEqual(l.notes, [
    {
      id: "66aa00000000000000000004",
      title: "鲑鱼跑 W3",
      xsec_token: "t4",
      author_id: CREATOR,
    },
  ]);
  const follow = {
    code: 0,
    data: {
      has_more: false,
      users: [
        { userid: CREATOR, nickname: "Grizzco Coach", fstatus: "follows" },
        { nickname: "nobody" },
      ],
    },
  };
  const f = rn.recognise(`${API}/v1/user/followings?cursor=`, follow);
  assert.equal(f.kind, "followings");
  assert.equal(f.has_more, false);
  assert.deepEqual(f.users, [{ user_id: CREATOR, nickname: "Grizzco Coach" }]);
  assert.deepEqual(
    rn.recognise(`${API}/v2/user/me`, {
      code: 0,
      success: true,
      data: { user_id: "me1", guest: false, nickname: "Me" },
    }),
    {
      kind: "me",
      user_id: "me1",
      guest: false,
      fields: ["guest", "nickname", "user_id"],
    },
  );
  // A guest has an id too, and may not say it is one
  assert.equal(
    rn.recognise(`${API}/v2/user/me`, {
      code: 0,
      success: true,
      data: { user_id: "g1", guest: false },
    }).guest,
    true,
  );
  assert.deepEqual(
    rn.recognise(`${API}/v1/feed`, {
      code: 300011,
      success: false,
      msg: "login required",
    }),
    {
      kind: "error",
      code: 300011,
      msg: "login required",
    },
  );
  assert.equal(rn.refused(300011), true);
  assert.equal(rn.refused(0), false);
  assert.equal(rn.recognise("https://www.xiaohongshu.com/x", { ok: 1 }), null);
  assert.equal(rn.recognise("https://www.xiaohongshu.com/x", null), null);
});

test("note ids come out of the page's links, with tokens and titles", () => {
  const listed = rn.listedFromLinks([
    [
      `https://www.xiaohongshu.com/user/profile/${CREATOR}/66aa00000000000000000005?xsec_token=T5&xsec_source=pc_user`,
      "Eggstra Work 500\nlikes 12",
    ],
    [
      "https://www.xiaohongshu.com/explore/66aa00000000000000000005?xsec_token=T5",
      "",
    ],
    ["/explore/66aa00000000000000000006", "Cat"],
    [`https://www.xiaohongshu.com/user/profile/${CREATOR}`, "profile"],
    ["https://www.xiaohongshu.com/explore/short", "no"],
    ["not a url at all::", "x"],
  ]);
  assert.deepEqual(listed, [
    {
      id: "66aa00000000000000000005",
      title: "Eggstra Work 500",
      xsec_token: "T5",
      author_id: CREATOR,
    },
    {
      id: "66aa00000000000000000006",
      title: "Cat",
      xsec_token: null,
      author_id: null,
    },
  ]);
});

test("a page that wants a person is told by its address or its words", () => {
  assert.match(
    rn.challenge(
      "https://www.xiaohongshu.com/website-login/captcha?redirectPath=x",
      "",
    ),
    /address holds "captcha"/,
  );
  assert.match(
    rn.challenge("https://www.xiaohongshu.com/explore/1", "请完成安全验证"),
    /says "安全验证"/,
  );
  assert.equal(
    rn.challenge("https://www.xiaohongshu.com/explore/1", "a note"),
    null,
  );
  assert.equal(rn.challenge(undefined, undefined), null);
  assert.ok(rn.PAGE.moreReplies.length);
  assert.ok(rn.isSiteApi(`${API}/v1/feed`));
  assert.ok(
    rn.isSiteApi("https://edith.xiaohongshu.com/api/sns/web/v2/user/me"),
  );
  assert.ok(!rn.isSiteApi("https://sns-img.xhscdn.com/1.jpg"));
});

test("creators are given as ids or profile links", () => {
  assert.equal(rn.creatorId(CREATOR), CREATOR);
  assert.equal(
    rn.creatorId(
      `https://www.xiaohongshu.com/user/profile/${CREATOR}?xsec_token=x`,
    ),
    CREATOR,
  );
  assert.throws(() => rn.creatorId("bob"), /not a creator/);
  assert.throws(() => rn.folderOf("../etc"), /not a creator id/);
});

test("a note read off the page becomes a note with its comments", () => {
  const note = rn.noteFromDom(
    {
      title: "Salmon Run W3 plan",
      desc: "Left side first.",
      author: "",
      date: "2025-01-02 上海",
      comments: [
        { author: "gil", text: "yes", date: "", reply: false },
        { author: "hal", text: "no", date: "", reply: true },
        { author: "", text: "", date: "", reply: false },
      ],
    },
    SR,
    { user_id: CREATOR, nickname: "Grizzco Coach" },
  );
  assert.equal(note.author.nickname, "Grizzco Coach");
  assert.equal(note.date, "2025-01-02T00:00:00.000Z");
  assert.equal(note.comments.length, 1);
  assert.equal(note.comments[0].replies[0].author.nickname, "hal");
  assert.equal(note.comment_count, 2);
  assert.equal(note.comments_complete, true);
  assert.equal(rn.noteFromDom(null, SR, {}), null);
});

test("notes are appended as JSON lines per creator", () => {
  const dir = mkdtempSync(join(tmpdir(), "rncap-test-"));
  const note = rn.recognise(`${API}/v1/feed`, feedFixture()).notes[0];
  note.comments = rn.recognise(
    `x?note_id=${SR}`,
    commentsFixture(false),
  ).comments;
  note.comments_complete = true;
  const line = rn.record(note, ["打工"], new Date("2026-09-27T00:00:00Z"));
  assert.equal(line.source, "rednote");
  assert.equal(line.id, SR);
  assert.equal("xsec_token" in line, false);
  assert.equal(line.comments[0].replies[0].reply_to, "c1");
  assert.equal(line.comments_complete, true);
  assert.equal(line.captured_at, "2026-09-27T00:00:00.000Z");
  const path = rn.append(dir, CREATOR, [line]);
  rn.append(dir, CREATOR, [{ ...line, id: "66aa00000000000000000009" }]);
  assert.equal(path, join(dir, CREATOR, "notes.jsonl"));
  const lines = readFileSync(path, "utf8")
    .trim()
    .split("\n")
    .map((l) => JSON.parse(l));
  assert.deepEqual(
    lines.map((l) => l.id),
    [SR, "66aa00000000000000000009"],
  );
  rmSync(dir, { recursive: true });
});

test("the site's hosts: xiaohongshu.com and rednote.com", () => {
  assert.equal(
    rn.originOf("https://www.rednote.com/user/profile/x"),
    "https://www.rednote.com",
  );
  assert.equal(
    rn.originOf("https://www.xiaohongshu.com"),
    "https://www.xiaohongshu.com",
  );
  assert.equal(rn.originOf("https://notrednote.com/"), null);
  assert.equal(rn.originOf("https://www.rednote.com.example.org/"), null);
  assert.ok(rn.isSiteApi("https://webapi.rednote.com/api/sns/web/v2/user/me"));
  assert.ok(rn.isSiteApi("https://edith.xiaohongshu.com/api/sns/web/v1/feed"));
  assert.ok(!rn.isSiteApi("https://www.rednote.com/explore"));
});
