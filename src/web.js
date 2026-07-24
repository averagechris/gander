(() => {
  "use strict";
  const body = document.body;
  const token = body.dataset.token;
  let generation = Number(body.dataset.generation);
  const mode = document.querySelector("#mode-switch");
  const search = document.querySelector("#review-search");

  mode?.addEventListener("click", () => {
    const full = body.dataset.mode !== "full";
    body.dataset.mode = full ? "full" : "guided";
    mode.textContent = full ? "Guided review" : "Full review";
    mode.setAttribute("aria-pressed", String(full));
    if (full) {
      document.querySelectorAll('.skim-region[data-full-loaded="false"]').forEach((region) => load(region, "full"));
    }
  });

  search?.addEventListener("input", () => {
    const query = search.value.trim().toLowerCase();
    document.querySelectorAll("[data-search]").forEach((node) => {
      node.hidden = query !== "" && !node.dataset.search.includes(query);
    });
  });

  const load = async (region, requestedMode = body.dataset.mode) => {
    if (!region.isConnected) return;
    if (!region.classList.contains("region-skeleton") && !(requestedMode === "full" && region.dataset.fullLoaded === "false")) return;
    const id = region.dataset.region;
    const query = new URLSearchParams({ token, generation: String(generation), mode: requestedMode });
    try {
      const response = await fetch(`/fragment/${encodeURIComponent(id)}?${query}`, {
        credentials: "same-origin",
        headers: { Accept: "text/html" },
      });
      if (!response.ok) throw new Error(await response.text());
      const template = document.createElement("template");
      template.innerHTML = await response.text();
      const replacement = template.content.firstElementChild;
      if (!replacement || replacement.dataset.region !== id) throw new Error("invalid fragment response");
      region.replaceWith(replacement);
    } catch (error) {
      region.classList.remove("region-skeleton");
      region.innerHTML = `<p class="fragment-error"></p>`;
      region.querySelector("p").textContent = `Could not load region: ${error.message}`;
    }
  };

  const skeletons = document.querySelectorAll(".region-skeleton");
  if ("IntersectionObserver" in window) {
    const observer = new IntersectionObserver((entries) => entries.forEach((entry) => {
      if (entry.isIntersecting) {
        observer.unobserve(entry.target);
        load(entry.target);
      }
    }), { rootMargin: "800px 0px" });
    skeletons.forEach((region) => observer.observe(region));
  } else {
    skeletons.forEach(load);
  }

  const parseFragment = (html, id) => {
    const template = document.createElement("template");
    template.innerHTML = html;
    const node = template.content.firstElementChild;
    if (!node || (node.dataset.region !== id && node.dataset.patchId !== id)) throw new Error("invalid SSE fragment");
    return node;
  };

  const reload = () => {
    try {
      sessionStorage.setItem("gander.reload", JSON.stringify({ mode: body.dataset.mode, scroll: scrollY }));
    } catch (_) {}
    location.assign(`/?${new URLSearchParams({ token })}`);
  };

  try {
    const saved = JSON.parse(sessionStorage.getItem("gander.reload") || "null");
    sessionStorage.removeItem("gander.reload");
    if (saved?.mode === "full") mode?.click();
    if (Number.isFinite(saved?.scroll)) requestAnimationFrame(() => scrollTo(0, saved.scroll));
  } catch (_) {}

  const source = new EventSource(`/events?${new URLSearchParams({ token, generation: String(generation) })}`);
  source.addEventListener("state", (message) => {
    try {
      const update = JSON.parse(message.data);
      if (!Number.isSafeInteger(update.generation) || update.generation <= generation) return;
      if (!update.full && update.generation !== generation + 1) return reload();
      const activeId = document.activeElement?.id;
      const anchor = [...document.querySelectorAll("[data-region]")].find((node) => node.getBoundingClientRect().bottom >= 0);
      const anchorId = anchor?.dataset.region;
      const anchorTop = anchor?.getBoundingClientRect().top;
      const ids = new Set(update.order || []);
      if (update.full) {
        document.querySelectorAll("#review-stream > [data-region]").forEach((node) => {
          if (!ids.has(node.dataset.region)) node.remove();
        });
      }
      for (const patch of update.patches || []) {
        const current = document.querySelector(`[data-region="${CSS.escape(patch.id)}"], [data-patch-id="${CSS.escape(patch.id)}"]`);
        if (patch.remove) {
          current?.remove();
          continue;
        }
        const html = body.dataset.mode === "full" ? patch.full : patch.guided;
        if (!html) return reload();
        const replacement = parseFragment(html, patch.id);
        if (current) current.replaceWith(replacement);
        else if (ids.has(patch.id)) document.querySelector("#review-stream")?.append(replacement);
        else return reload();
      }
      const stream = document.querySelector("#review-stream");
      for (const id of update.order || []) {
        const node = stream?.querySelector(`[data-region="${CSS.escape(id)}"]`);
        if (node) stream.append(node);
      }
      generation = update.generation;
      body.dataset.generation = String(generation);
      if (activeId) document.getElementById(activeId)?.focus({ preventScroll: true });
      if (anchorId && Number.isFinite(anchorTop)) {
        const moved = document.querySelector(`[data-region="${CSS.escape(anchorId)}"]`);
        if (moved) scrollBy(0, moved.getBoundingClientRect().top - anchorTop);
      }
    } catch (_) {
      reload();
    }
  });
})();
