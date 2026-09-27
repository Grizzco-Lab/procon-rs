// Whether a post is about Salmon Run: a glossary match in English,
// Japanese and Simplified Chinese (the official names of the mode, its
// events, bosses, Kings, stages and jargon). The account follows Salmon
// Run creators already, so this only keeps their other posts out of the
// store; a false positive costs a document, a false negative a post.

/** Terms in scripts without word boundaries: matched as substrings */
export const CJK_TERMS = [
  // The mode and its parts
  "サーモンラン",
  "サモラン",
  "バイト",
  "鮭",
  "シャケ",
  "クマサン",
  "金イクラ",
  "イクラ",
  "オオモノ",
  "オカシラ",
  "キケン度",
  "カンスト",
  "でんせつ",
  "ビッグラン",
  "バイトチームコンテスト",
  "EX-WAVE",
  "満潮",
  "干潮",
  "納品",
  // Bosses, lessers and Kings (Japanese)
  "バクダン",
  "カタパッド",
  "ヘビ",
  "テッパン",
  "タワー",
  "モグラ",
  "コウモリ",
  "ハシラ",
  "ダイバー",
  "テッキュウ",
  "ナベブタ",
  "コジャケ",
  "ドスコイ",
  "タマヒロイ",
  "キンシャケ",
  "グリル",
  "ドロシャケ",
  "ヨコヅナ",
  "タツ",
  "ジョー",
  // Events and stages (Japanese)
  "ラッシュ",
  "ハコビヤ",
  "巨大タツマキ",
  "ドスコイ大量発生",
  "ドロシャケ噴出",
  "キンシャケ探し",
  "シェケナダム",
  "難破船",
  "ドン・ブラコ",
  "ムニ・エール",
  "アラマキ砦",
  "すじこジャンクション",
  "トキシラズ",
  "どんぴこ闘技場",
  // Simplified Chinese
  "打工",
  "鲑鱼跑",
  "鲑鱼",
  "熊先生",
  "金鲑鱼卵",
  "金蛋",
  "鲑鱼卵",
  "头目",
  "危险度",
  "大型跑",
  "团队打工竞赛",
  "满潮",
  "干潮",
  "炸弹鱼",
  "垫肩飞鱼",
  "蛇鱼",
  "铁板鱼",
  "高塔鱼",
  "鼹鼠鱼",
  "蝙蝠鱼",
  "柱鱼",
  "潜水鱼",
  "铁球鱼",
  "锅盖鱼",
  "小鲑鱼",
  "小偷鱼",
  "金鲑鱼",
  "横纲",
  "辰龙",
  "巨颚",
  "鲑坝",
  "漂浮落难船",
  "麦年海洋发电所",
  "新卷堡",
  "生筋子",
  "烟熏工房",
  "斗技场",
  // Simplified Chinese jargon, as the glossary's aliases have it: the
  // stages' short names, the shop and its weapons, carrying eggs
  "鬼坝",
  "破船",
  "落难船",
  "发电所",
  "熊商会",
  "熊武",
  "熊刷",
  "搬蛋",
  "运蛋",
  "外围蛋",
  "蛋筐",
  "筐边",
];

/** Terms in Latin script: matched as whole words, any case */
export const LATIN_TERMS = [
  "salmon run",
  "salmonid",
  "salmonids",
  "grizzco",
  "grizz",
  "eggstra",
  "eggstra work",
  "big run",
  "golden egg",
  "golden eggs",
  "power egg",
  "power eggs",
  "egg basket",
  "hazard level",
  "eggsecutive",
  "evp",
  "xtrawave",
  "overfishing",
  "king salmonid",
  "steelhead",
  "flyfish",
  "scrapper",
  "steel eel",
  "stinger",
  "maws",
  "drizzler",
  "fish stick",
  "flipper-flopper",
  "flipper flopper",
  "big shot",
  "slammin' lid",
  "slammin lid",
  "smallfry",
  "chum",
  "cohock",
  "cohocks",
  "snatcher",
  "snatchers",
  "goldie",
  "goldies",
  "griller",
  "grillers",
  "mudmouth",
  "mudmouths",
  "cohozuna",
  "horrorboros",
  "megalodontia",
  "triumvirate",
  "glowflies",
  "mothership",
  "goldie seeking",
  "cohock charge",
  "giant tornado",
  "mudmouth eruptions",
  "spawning grounds",
  "marooner's bay",
  "gone fission",
  "hydroplant",
  "sockeye station",
  "jammin' salmon junction",
  "salmonid smokeyard",
  "bonerattle arena",
];

const latin = new RegExp(
  "(?<![a-z0-9])(?:" +
    LATIN_TERMS.map((t) => t.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")).join("|") +
    ")(?![a-z0-9])",
  "gi",
);

/** The Salmon Run terms `text` mentions, as written in the lists above;
 * empty when none */
export function matches(text) {
  const normal = (text ?? "").normalize("NFKC");
  const found = new Set();
  const lower = normal.toLowerCase();
  for (const term of CJK_TERMS) {
    if (lower.includes(term.toLowerCase())) found.add(term);
  }
  for (const m of lower.matchAll(latin)) found.add(m[0]);
  return [...found];
}

/** Whether a post (its text, and its quoted post's) is about Salmon Run */
export function isSalmonRun(post) {
  const text = [post?.text, post?.quoted?.text].filter(Boolean).join("\n");
  return matches(text).length > 0;
}
