(() => {
  "use strict";
  const body = document.body;
  const token = body.dataset.token;
  const generation = body.dataset.generation;
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
    const query = new URLSearchParams({ token, generation, mode: requestedMode });
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
})();
