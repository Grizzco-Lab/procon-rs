// Demo questions for the Cuttlefish chat (cuttlefish.js shows a few at
// random as chips next to the review examples, in the page's language).
// Each is answered by the fact cards `cuttlefish ingest leanny` imports
// from Lean's Splatoon 3 datamine (leanny.github.io) with exact numbers,
// checked against the data of September 2026 (game version 11.3.0, Eggstra
// Work events 1 to 14); the last ones in each list also need the
// #vod-review knowledge. Edit freely: the file is data, served as
// /demo-questions.js.
"use strict";

window.DEMO_QUESTIONS = {
  en: [
    // Salmonids (CoopEnemyInfo)
    "How many Steelheads can be on the field at once, and how many power eggs does one give per hit?",
    "How many power eggs does a Flyfish drop per hit and on a kill?",
    "How many Stingers can be on the field at once?",
    "How many Goldies can be active at the same time in Goldie Seeking?",
    "How many power eggs does a hit on Cohozuna give, and how does its HP coefficient change with the hazard level?",
    "What are the Japanese and Simplified Chinese names of the Slammin' Lid?",
    // Hazard levels (CoopLevelsConfig)
    "What is the Mothership's HP at hazard level 333%?",
    "How fast are the lesser Salmonids in Rush at hazard level 200% compared with 333%?",
    "How many golden eggs does each Giant Tornado box hold at hazard level 333%?",
    "What are the golden egg quotas per wave (NormaGoldenIkuraNum) at difficulty 1665?",
    // Weapons and stages
    "What damage does the Splattershot do in Salmon Run according to the game data?",
    "Which weapons are Grizzco weapons in the game data?",
    "Which Salmon Run stages are Big Run stages?",
    // Eggstra Work (scenarios and dates)
    "What spawned in wave 3 of Eggstra Work #7?",
    "Which Eggstra Work used Marooner's Bay, and with which weapons and specials?",
    "When was Eggstra Work #7, on which stage, and what were its five waves?",
    "What were the tides and occurrences of the five waves of Eggstra Work #9?",
    "Where did the Snatcher spawn in wave 2 of Eggstra Work #5?",
    "What were the high score thresholds for the top 5%, 20% and 50% in Eggstra Work #12?",
    "Which Eggstra Work events were reruns of earlier scenarios?",
    // Fact cards plus the #vod-review knowledge
    "Eggstra Work #7 wave 3 was a standard wave on Bonerattle Arena: list its first bosses, and what do the #vod-review reviewers say about opening a wave there?",
    "Eggstra Work #4 had the Dynamo Roller on Marooner's Bay: what were its waves, and what do reviewers say about playing Dynamo there?",
    "How fast are lessers in Rush at hazard level 333% in the game data, and where do the #vod-review reviewers say to stand for Rush?",
    "Which Eggstra Work events had a Mothership wave, and what do reviewers say about handling the Mothership?",
  ],
  zh: [
    "炸弹鱼最多同时在场几只？打中一次掉多少鲑鱼卵？",
    "垫肩飞鱼每次命中和击杀分别掉多少鲑鱼卵？",
    "高塔鱼最多同时在场几只？",
    "寻找金鲑鱼时最多同时有几只金鲑鱼？",
    "打中横纲一次掉多少鲑鱼卵？它的 HP 系数随危险度怎么变？",
    "锅盖鱼的日文名和英文名是什么？",
    "危险度 333% 时头目船（The Mothership）的 HP 是多少？",
    "危险度 200% 和 333% 时，狂潮（Rush）里小鲑鱼的速度系数分别是多少？",
    "危险度 333% 时巨大龙卷风每个箱子有几颗金鲑鱼卵？",
    "斯普拉射击枪在鲑鱼跑里的伤害是多少（游戏数据）？",
    "游戏数据里哪些武器是熊先生商会的武器？",
    "第 7 届团队打工竞赛第 3 波刷了什么？",
    "哪一届团队打工竞赛用了漂浮落难船？武器和特殊武器是什么？",
    "第 7 届团队打工竞赛是什么时候、在哪个场地、五波分别是什么？",
    "第 9 届团队打工竞赛五波的潮位和事件分别是什么？",
    "第 12 届团队打工竞赛前 5%、20%、50% 的高分门槛是多少？",
    // 数据卡片加上 #vod-review 的经验
    "第 7 届团队打工竞赛第 3 波在鲑鱼心脏斗技场是普通波：先出的是哪些巨大鲑鱼？#vod-review 的高手对这张图开波怎么说？",
    "第 4 届团队打工竞赛在漂浮落难船有电动马达滚筒：五波是什么？高手对在这张图玩滚筒有什么建议？",
    "危险度 333% 时狂潮里小鲑鱼有多快？#vod-review 的高手建议狂潮站在哪里？",
  ],
};
