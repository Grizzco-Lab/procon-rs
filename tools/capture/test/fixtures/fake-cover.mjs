// A stand-in for cover.py in the tests: the same line protocol, the score
// read off the picture's bytes ("SPLAT…" is the game, "BAD" an error,
// anything else not the game)
import { createInterface } from "node:readline";

console.log(JSON.stringify({ ready: true, model: "fake", device: "cpu" }));
createInterface({ input: process.stdin }).on("line", (line) => {
  const { id, image } = JSON.parse(line);
  const bytes = Buffer.from(image, "base64").toString();
  if (bytes.startsWith("BAD"))
    console.log(JSON.stringify({ id, error: "UnidentifiedImageError: no" }));
  else
    console.log(
      JSON.stringify({ id, p: bytes.startsWith("SPLAT") ? 0.9 : 0.1 }),
    );
});
