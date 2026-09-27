# Recording checklist

A short pre-flight list for a recording day (Salmon Run with the Studio app).
The README's "Studio" and "Recordings" sections explain each setting.

## Before the first session

1. **Pi proxy up.** `./scripts/deploy.sh` (or `sudo ./procon-proxy` on the Pi)
   restarts the proxy, which resets the Pro Controller over sysfs at start. In
   the top bar, **Proxy** and **Controller** are green and the Motion panel
   shows a steady input rate (Hz). "No controller" while the Switch reacts
   means the controller went Bluetooth: replug it and restart the proxy.
2. **Capture card input.** Video panel: the input is the Elgato
   (`/dev/video0`), not "Screen" or "No video", and the preview moves. A busy
   device means OBS (or another program) holds it; close it.
3. **Sound.** "Record sound" is on and its note says "Sound arriving". Without
   sound arriving the files are recorded without a sound track.
4. **Quality.** Record at **720p, 30 fps** for now (the first sessions were
   360p; 1080p takes too much space). Everything downstream handles mixed
   sizes: the IDM scales frames to 640x360, the HUD reader and labeling work
   at any size. Change it only between sessions.
5. **Game settings.** Motion controls on, motion sensitivity **4.5**, stick
   sensitivity **5**, no inversion, matching Splatoon 3's options. They are
   saved into each session's `session.json` and cannot change while recording.
6. **Path prefix.** "Save to" is the Dataset folder:
   `/home/cjr/DropboxRemote/SalmonRun/Dataset/`. "Next session" shows the
   folder the next Record creates.
7. **Disk space.** The Data panel's disk meter, and once recording, its rate
   per hour and "Room for about …". Keep tens of GB free, and let Dropbox
   sync.
8. **No GPU jobs.** Stop training, prediction and other GPU work (AgentZero,
   Vision, Predictor runs) while recording: the preview and the recording
   encoder use NVENC, and a busy GPU delays frames. Check with `nvidia-smi`.
   Heavy CPU jobs (builds, other agents) also slow the grabber: frames then
   queue up and the end of a video file comes up short.

## While recording

- Record, Pause and Resume (each resume starts the next `video-NN.mkv`), Stop.
- The top bar's REC chip shows the time in every app; the Data panel shows
  "No frames dropped" and the Proxy chip's latency stays low.
- A red notice under the Recording panel means the session ended (disk full,
  for one); press Record again.

### Marking techniques

Practice sessions of advanced movement get technique markers: labelled
examples for the IDM, and a reminder of what is still missing.

1. Before recording, open the Techniques panel's **Checklist**: techniques
   with ○ have no example in any session yet.
2. Pick the technique to practise: click it, or press its number (1–9) while
   the Studio is shown (not while typing in a field).
3. Record, then for each rep either
   - press **M** when it starts and **M** again when it ends (a span; the
     panel shows "Marking … 00:03" in red while it runs), or
   - do it, then press **B** to mark the last N seconds (the box next to
     **Mark last**, 5 s by default; keep it a little longer than the rep).
4. A wrong key: **U** undoes the last marker (or drops the open span).
   Starting another technique ends the open span; Pause and Stop end it too.
5. **This session** counts the reps marked so far. Markers go to the
   session's `session.json` at once, so a crash keeps them.
6. Afterwards, the Inkspector shows them on the scrubber; fix a start or end
   there, or add a marker after the fact with **Add marker here**.

A technique missing from the list: type its name (and its Chinese name and
Pedia term id, if any) under the list and press **Add**; it stays on the
list across restarts.

## After recording

1. Open the new session in the **Inkspector**: the video plays, the input
   overlay moves with the picture, the sound plays.
2. **Calibration**: measure the new sessions' video delay in AgentZero,
   `uv run agentzero-calibrate --all /home/cjr/DropboxRemote/SalmonRun/Dataset`
   (writes `calibration.json`; set a delay by hand in the Inkspector where the
   estimate has low confidence).
3. **Refresh the IDM data**: run AgentZero's refresh command (splits,
   controllable frames, turn fits) over the Dataset before the next training
   run.
