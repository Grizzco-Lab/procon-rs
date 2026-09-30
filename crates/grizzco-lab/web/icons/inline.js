// Inline the icon set. Some browsers (Safari) do not draw
// `<use href="/icons/NAME.svg#i">` from another file, so each icon used on
// the page is fetched once, copied into a hidden sprite as `#icon-NAME`, and
// the `<use>` is pointed at that copy. Icons added later are handled too.
(() => {
  const PATTERN = /(?:^|\/)([\w-]+)\.svg#i$/;
  const symbols = new Map();
  let sprite = null;

  /** The hidden sprite holding the copied icons */
  function spriteElement() {
    if (!sprite) {
      sprite = document.createElementNS("http://www.w3.org/2000/svg", "svg");
      sprite.setAttribute("aria-hidden", "true");
      sprite.style.cssText =
        "position:absolute;width:0;height:0;overflow:hidden";
      document.body.prepend(sprite);
    }
    return sprite;
  }

  /** Fetch icon `name` once and copy its drawing into the sprite */
  function symbol(name) {
    if (!symbols.has(name)) {
      symbols.set(
        name,
        fetch(`/icons/${name}.svg`)
          .then((response) => response.text())
          .then((text) => {
            const svg = new DOMParser().parseFromString(text, "image/svg+xml");
            const drawing = svg.getElementById("i");
            if (!drawing) throw new Error(`icon ${name} has no #i`);
            const copy = document.createElementNS(
              "http://www.w3.org/2000/svg",
              "symbol",
            );
            copy.id = `icon-${name}`;
            copy.setAttribute(
              "viewBox",
              svg.documentElement.getAttribute("viewBox") ?? "0 0 24 24",
            );
            drawing.removeAttribute("id");
            copy.append(document.importNode(drawing, true));
            spriteElement().append(copy);
          }),
      );
    }
    return symbols.get(name);
  }

  /** Point every file `<use>` under `root` at its inlined copy */
  function inline(root) {
    const uses = root.matches?.("use") ? [root] : [];
    uses.push(...(root.querySelectorAll?.("use") ?? []));
    for (const use of uses) {
      const match = PATTERN.exec(use.getAttribute("href") ?? "");
      if (!match) continue;
      const name = match[1];
      symbol(name).then(
        () => use.setAttribute("href", `#icon-${name}`),
        (error) => console.warn(`icon ${name}:`, error),
      );
    }
  }

  function start() {
    inline(document.body);
    new MutationObserver((records) => {
      for (const record of records) {
        for (const node of record.addedNodes) inline(node);
      }
    }).observe(document.body, { childList: true, subtree: true });
  }

  if (document.body) start();
  else document.addEventListener("DOMContentLoaded", start);
})();
