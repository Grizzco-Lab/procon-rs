# Open questions

Decisions nobody has made yet. Each has a suggested answer; the code works
without deciding. Remove an entry once it is decided (and note where the
decision lives).

## Vision (`crates/gameplay-vision`)

1. **COCO weights under AGPL.** The pretrained YOLOv8 weights are converted
   from Ultralytics (AGPL-3.0). Fine for experiments, since they are
   downloaded at run time and never shipped? Suggested: experiments only;
   our own models are trained from scratch.
2. **Frames outside a wave** (lobby, results screen, practice area): exclude
   them from labeling, or keep them as frames with nothing to find?
   Suggested: keep a few as negatives, tag the rest out.
3. **Alert icons and names drawn over the scene**: label them or skip them?
   Suggested: skip, and leave the HUD alone.
4. **Where detector training runs**: AgentZero (PyTorch) or elsewhere?
   Suggested: AgentZero, as a teacher model for `prelabel`.
5. **3D stage geometry**: extracted from the game, or only what we build
   ourselves from recordings? Suggested: only our own.
6. **Boxing Steel Eel and Fish Stick**: one box for the whole boss or boxes
   for its parts?

## Labeling (Inkspector's Label mode)

7. **Frames checked and found empty**: keep them as `{"frame": n, "boxes": []}`
   so they count as labeled? Today deleting every box removes the line.
8. **Accepting a model box** drops its `score`. Keep it?

## Cuttlefish

9. **Sources** to supply: the Overfishing wiki's address, the "Next Wave:
   Overfishing Fundamentals" file, Lenny's repository.
10. **Discord #vod-review**: the admins must agree to a bot
    (`DISCORD_BOT_TOKEN`) or provide exports. Anonymize author names?
11. **Inkipedia**: its robots.txt blocks AI crawlers; ask its admins or use a
    dump they publish (CC BY-NC-SA).
12. **Glossary**: Chinese, Spanish, Russian and French names should come from
    the community.
13. **`digest.md`**: the short fundamentals summary sent with every request
    is still to be written.
14. **YouTube ranges** start at a keyframe near `start_s`;
    `--force-keyframes-at-cuts` would make them exact but re-encodes.
15. **Comment author**: saved as `user` and shown as "You"; use a real name?
16. **Object label code** lives in `src/objects.rs` and again in
    `gameplay-vision`; move one reader into `gameplay-data` (shared with
    Python)?

## Vision app

17. **Tracker settings with a frame step**: with every 2nd frame, most COCO
    tracks last only 1–4 frames. Scale the tracker's age/hits with the step,
    or expose them in the UI?
18. **Minimum score for "Send to labels"**: today every box above 0.25 is
    written. Add a field?
19. **Run history**: only the last run per segment is kept. Keep several (for
    example CPU vs GPU on the same range)?

## Cuttlefish knowledge

20. **Deleting documents**: the crate can't delete a document yet, so the
    Knowledge tab has no delete. Add it?
