(() => {
  "use strict";
  const body = document.body;
  const mode = document.querySelector("#mode-switch");
  const setMode = (value) => {
    const full = value === "full";
    body.dataset.mode = full ? "full" : "guided";
    mode.textContent = full ? "Guided review" : "Full review";
    mode.setAttribute("aria-pressed", String(full));
  };
  mode?.addEventListener("click", () => setMode(body.dataset.mode === "full" ? "guided" : "full"));
  document.addEventListener("click", (event) => {
    const button = event.target.closest?.("button[data-local-action]");
    if (button?.dataset.localAction === "fold-toggle") {
      const region = button.closest(".skim-region");
      const expanded = button.getAttribute("aria-expanded") !== "true";
      button.setAttribute("aria-expanded", String(expanded));
      button.textContent = expanded ? "⌃" : "⌄";
      region?.classList.toggle("present-expanded", expanded);
    }
    if (button?.dataset.localAction === "context-toggle") {
      const region = button.closest(".file-region");
      const collapsed = region?.classList.toggle("context-collapsed");
      button.setAttribute("aria-expanded", String(!collapsed));
      button.textContent = collapsed ? "Expand context" : "Collapse context";
    }
    const nav = event.target.closest?.("[data-guide-nav]");
    if (nav) {
      const stops = [...document.querySelectorAll("[data-step-id]")];
      if (!stops.length) return;
      const current = stops.findIndex((stop) => stop.closest(".annotation")?.getBoundingClientRect().top >= 0);
      const delta = nav.dataset.guideNav === "prev" ? -1 : 1;
      stops[Math.max(0, Math.min(stops.length - 1, (current < 0 ? 0 : current) + delta))]
        ?.closest(".annotation")?.scrollIntoView({ block: "center" });
    }
  });
})();
