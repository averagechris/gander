(() => {
  "use strict";
  const body = document.body;
  const token = body.dataset.token;
  let generation = Number(body.dataset.generation);
  const mode = document.querySelector("#mode-switch");
  const search = document.querySelector("#review-search");
  const presenter = document.querySelector("#presenter");
  const presenterStatus = document.querySelector("#presenter-status");
  const rejoin = document.querySelector("#presenter-rejoin");
  const edge = document.querySelector("#presenter-edge");
  const note = document.querySelector("#presenter-note");
  const reducedMotion = matchMedia("(prefers-reduced-motion: reduce)");
  const tabId = (() => {
    try {
      const prior = sessionStorage.getItem("gander.tabId");
      if (prior) return prior;
      const id = crypto.randomUUID();
      sessionStorage.setItem("gander.tabId", id);
      return id;
    } catch (_) {
      return `tab-${Date.now()}-${Math.random().toString(36).slice(2)}`;
    }
  })();

  let presentation = { active: false, following: false, target: null, ownsMode: false, priorMode: null };
  let presenterRows = [];
  let suppressScroll = false;
  let reportTimer;
  const pendingActions = new Set();
  let returnedGeneration = generation;

  const setMode = (next, human = false) => {
    const full = next === "full";
    body.dataset.mode = full ? "full" : "guided";
    mode.textContent = full ? "Guided review" : "Full review";
    mode.setAttribute("aria-pressed", String(full));
    if (full) {
      document.querySelectorAll('.skim-region[data-full-loaded="false"]').forEach((region) => load(region, "full"));
    }
    if (human && presentation.active) {
      presentation.ownsMode = false;
      pauseFollow();
    }
  };

  mode?.addEventListener("click", () => setMode(body.dataset.mode === "full" ? "guided" : "full", true));

  search?.addEventListener("input", () => {
    const query = search.value.trim().toLowerCase();
    document.querySelectorAll("[data-search]").forEach((node) => {
      node.hidden = query !== "" && !node.dataset.search.includes(query);
    });
    reportInteraction("search");
  });
  search?.addEventListener("focus", () => reportInteraction("search"));
  search?.addEventListener("blur", () => reportInteraction(null));

  const load = async (region, requestedMode = body.dataset.mode) => {
    if (!region?.isConnected) return region;
    if (!region.classList.contains("region-skeleton") && !(requestedMode === "full" && region.dataset.fullLoaded === "false")) return region;
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
      return replacement;
    } catch (error) {
      region.classList.remove("region-skeleton");
      region.innerHTML = `<p class="fragment-error"></p>`;
      region.querySelector("p").textContent = `Could not load region: ${error.message}`;
      return region;
    }
  };

  const observeSkeletons = () => {
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
  };
  observeSkeletons();

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
    if (saved?.mode === "full") setMode("full");
    if (Number.isFinite(saved?.scroll)) requestAnimationFrame(() => scrollTo(0, saved.scroll));
  } catch (_) {}

  const visibleFocus = () => {
    const selected = document.querySelector(".diff-row.web-selected");
    const rows = [...document.querySelectorAll(".diff-row[data-path]")];
    const row = selected || rows.find((candidate) => {
      const rect = candidate.getBoundingClientRect();
      return rect.bottom >= 72 && rect.top <= innerHeight * 0.45;
    });
    if (!row) return { path: null, old_line: null, new_line: null, hunk_header: null, pane: "diff" };
    return {
      path: row.dataset.path || null,
      old_line: row.dataset.oldLine ? Number(row.dataset.oldLine) : null,
      new_line: row.dataset.newLine ? Number(row.dataset.newLine) : null,
      hunk_header: row.dataset.hunk || null,
      pane: "diff",
    };
  };

  const reportInteraction = (busy = null, focus = visibleFocus()) => {
    clearTimeout(reportTimer);
    reportTimer = setTimeout(() => {
      fetch(`/interaction?${new URLSearchParams({ token })}`, {
        method: "POST",
        credentials: "same-origin",
        headers: { "Content-Type": "application/json", Accept: "application/json" },
        body: JSON.stringify({ tab_id: tabId, focus, busy }),
        keepalive: true,
      }).catch(() => {});
    }, 80);
  };

  document.addEventListener("click", (event) => {
    const row = event.target.closest?.(".diff-row[data-path]");
    document.querySelector(".diff-row.web-selected")?.classList.remove("web-selected");
    if (row) {
      row.classList.add("web-selected");
      reportInteraction(null, visibleFocus());
    }
    const file = event.target.closest?.("#file-tree a");
    if (file) reportInteraction(null, { path: file.textContent.trim(), old_line: null, new_line: null, hunk_header: null, pane: "files" });
  });

  const actionStatus = (message, error = false) => {
    const status = document.querySelector(".action-status");
    if (status) {
      status.textContent = message || "";
      status.classList.toggle("action-error", error);
    }
  };

  const postAction = async (verb, payload, owner, optimistic = () => () => {}) => {
    const key = `${verb}:${payload.id || payload.path || payload.fold_id || "one"}`;
    if (pendingActions.has(key)) return null;
    pendingActions.add(key);
    const rollback = optimistic() || (() => {});
    owner?.classList.add("action-pending");
    actionStatus("Saving…");
    try {
      const response = await fetch(`/actions/${verb}?${new URLSearchParams({ token })}`, {
        method: "POST",
        credentials: "same-origin",
        headers: { "Content-Type": "application/json", Accept: "application/json" },
        body: JSON.stringify({ expected_generation: generation, ...payload }),
      });
      if (!response.ok) {
        const message = await response.text();
        rollback();
        owner?.classList.toggle("action-conflict", response.status === 409);
        actionStatus(message, true);
        if (response.status === 409) setTimeout(reload, 350);
        return null;
      }
      const value = await response.json();
      returnedGeneration = Math.max(returnedGeneration, Number(value.generation) || generation);
      actionStatus("Saved");
      if (returnedGeneration > generation) setTimeout(() => { if (generation < returnedGeneration) reload(); }, 1500);
      return value.result;
    } catch (error) {
      rollback();
      actionStatus(`Could not save: ${error.message}`, true);
      return null;
    } finally {
      pendingActions.delete(key);
      owner?.classList.remove("action-pending");
    }
  };

  const selectedTarget = (fallbackPath = null) => {
    const focus = visibleFocus();
    const path = fallbackPath && focus.path !== fallbackPath ? fallbackPath : (focus.path || fallbackPath);
    const line = focus.new_line || focus.old_line;
    return path ? { path, ...((!fallbackPath || focus.path === fallbackPath) && line ? { line } : {}) } : null;
  };

  document.addEventListener("change", (event) => {
    const checkbox = event.target.closest?.('[data-action="file-viewed"]');
    if (!checkbox) return;
    const viewed = checkbox.checked;
    postAction(viewed ? "file-viewed" : "file-unviewed", { path: checkbox.dataset.path }, checkbox.closest("li"), () => () => { checkbox.checked = !viewed; });
  });

  document.addEventListener("submit", async (event) => {
    const composer = event.target.closest?.("#comment-composer");
    const reply = event.target.closest?.(".reply-form");
    const editor = event.target.closest?.(".comment-edit-form");
    if (!composer && !reply && !editor) return;
    event.preventDefault();
    const data = new FormData(event.target);
    const bodyText = String(data.get("body") || "");
    if (!bodyText.trim()) return;
    reportInteraction("comment editor");
    let result;
    if (composer) {
      const target = selectedTarget();
      const channel = String(data.get("channel") || "");
      const sourceCommentId = composer.dataset.sourceCommentId;
      result = await postAction("comment-add", { ...(target || {}), body: bodyText, ...(channel ? { channel } : {}), ...(sourceCommentId ? { source_comment_id: sourceCommentId } : {}) }, composer);
    } else if (reply) {
      result = await postAction("comment-reply", { id: reply.dataset.commentId, body: bodyText }, reply);
    } else {
      result = await postAction("comment-edit", { id: editor.dataset.commentId, body: bodyText }, editor);
    }
    if (result) {
      event.target.reset();
      if (composer) {
        delete composer.dataset.sourceCommentId;
        composer.querySelector("textarea").placeholder = "Leave durable review feedback…";
      }
      if (editor) editor.remove();
    }
    reportInteraction(null);
  });

  document.addEventListener("focusin", (event) => {
    if (event.target.closest?.("#comment-composer,.reply-form,.comment-edit-form")) reportInteraction("comment editor");
  });
  document.addEventListener("focusout", (event) => {
    if (event.target.closest?.("#comment-composer,.reply-form,.comment-edit-form")) setTimeout(() => {
      if (!document.activeElement?.closest?.("#comment-composer,.reply-form,.comment-edit-form")) reportInteraction(null);
    }, 0);
  });

  document.addEventListener("click", async (event) => {
    const button = event.target.closest?.("button[data-action],button[data-local-action]");
    if (!button) return;
    const action = button.dataset.action || button.dataset.localAction;
    if (action === "fold-toggle") {
      const region = button.closest(".skim-region");
      const expanded = button.getAttribute("aria-expanded") !== "true";
      button.setAttribute("aria-expanded", String(expanded));
      button.textContent = expanded ? "⌃" : "⌄";
      if (expanded) {
        const loaded = await load(region, "full");
        loaded.classList.add("present-expanded");
      } else region.classList.remove("present-expanded");
      return;
    }
    if (action === "context-toggle") {
      const region = button.closest(".file-region");
      const collapsed = region.classList.toggle("context-collapsed");
      button.setAttribute("aria-expanded", String(!collapsed));
      button.textContent = collapsed ? "Expand context" : "Collapse context";
      return;
    }
    if (action === "comment-edit") {
      const article = button.closest(".annotation");
      if (article.querySelector(".comment-edit-form")) return;
      const form = document.createElement("form");
      form.className = "comment-edit-form reply-form";
      form.dataset.commentId = button.dataset.commentId;
      const textarea = document.createElement("textarea");
      textarea.name = "body";
      textarea.required = true;
      textarea.value = button.dataset.commentBody || "";
      const save = document.createElement("button");
      save.type = "submit";
      save.textContent = "Save edit";
      form.append(textarea, save);
      article.append(form);
      textarea.focus();
      return;
    }
    if (action === "comment-request") {
      const composer = document.querySelector("#comment-composer");
      composer.dataset.sourceCommentId = button.dataset.commentId;
      const textarea = composer.querySelector("textarea");
      textarea.placeholder = "Ask the agent about this onboarding annotation…";
      composer.scrollIntoView({ block: "center" });
      textarea.focus();
      return;
    }
    if (action === "skim-acknowledge") {
      postAction(action, { fold_id: button.dataset.foldId }, button.closest(".skim-region"), () => {
        const prior = button.textContent;
        button.disabled = true; button.textContent = "✓ Acknowledged";
        return () => { button.disabled = false; button.textContent = prior; };
      });
      return;
    }
    if (action === "comment-state") {
      const article = button.closest(".annotation");
      postAction(action, { id: button.dataset.commentId, state: button.dataset.state }, article, () => {
        const prior = button.textContent; button.textContent = "Saved";
        return () => { button.textContent = prior; };
      });
      return;
    }
    if (action === "draft-accept" || action === "draft-discard") {
      const article = button.closest(".annotation");
      postAction(action, { id: button.dataset.commentId }, article, () => {
        article.hidden = true; return () => { article.hidden = false; };
      });
      return;
    }
    if (["salience-promote", "salience-demote", "salience-set", "salience-clear"].includes(action)) {
      const target = selectedTarget(button.dataset.path);
      if (target) postAction(action, { target, ...(button.dataset.salience ? { salience: button.dataset.salience } : {}) }, button.closest(".region"), () => {
        const row = document.querySelector(".diff-row.web-selected");
        const owner = row && row.dataset.path === target.path ? row : button.closest(".region");
        const prior = ["spotlight", "supporting", "skim"].find((name) => owner?.classList.contains(`salience-${name}`));
        const order = ["skim", "supporting", "spotlight"];
        let next = button.dataset.salience;
        if (!next && prior && action === "salience-promote") next = order[Math.min(order.length - 1, order.indexOf(prior) + 1)];
        if (!next && prior && action === "salience-demote") next = order[Math.max(0, order.indexOf(prior) - 1)];
        if (next) {
          order.forEach((name) => owner?.classList.remove(`salience-${name}`));
          owner?.classList.add(`salience-${next}`);
        }
        owner?.classList.add("action-pending");
        return () => {
          order.forEach((name) => owner?.classList.remove(`salience-${name}`));
          if (prior) owner?.classList.add(`salience-${prior}`);
          owner?.classList.remove("action-pending");
        };
      });
      return;
    }
    if (action === "walkthrough-next" || action === "walkthrough-prev") {
      const destination = await postAction(action, {}, button);
      if (destination?.step_id) document.querySelector(`[data-step-id="${CSS.escape(destination.step_id)}"]`)?.closest(".annotation")?.scrollIntoView({ block: "center" });
    }
  });

  const clearPresenterTarget = () => {
    presenterRows.forEach((row) => row.classList.remove("present-target", "present-target-static"));
    presenterRows = [];
    document.querySelector(".present-expanded")?.classList.remove("present-expanded");
  };

  const targetRows = (target) => {
    const escaped = CSS.escape(target.path || "");
    return [...document.querySelectorAll(`.diff-row[data-path="${escaped}"]`)].filter((row) => {
      if (!Number.isSafeInteger(target.line)) return true;
      const oldLine = Number(row.dataset.oldLine);
      const newLine = Number(row.dataset.newLine);
      const end = Number.isSafeInteger(target.end_line) ? target.end_line : target.line;
      return (Number.isSafeInteger(oldLine) && target.line <= oldLine && oldLine <= end)
        || (Number.isSafeInteger(newLine) && target.line <= newLine && newLine <= end);
    });
  };

  const materializeTarget = async (target) => {
    if (!target) return null;
    let region = target.region_id && document.querySelector(`[data-region="${CSS.escape(target.region_id)}"]`);
    if (region?.classList.contains("region-skeleton")) region = await load(region, "guided");
    let row = target.row_id && document.getElementById(target.row_id);
    if (!row && region?.classList.contains("skim-region")) {
      region = await load(region, "full");
      region.classList.add("present-expanded");
      row = target.row_id && document.getElementById(target.row_id);
    }
    return row || region || null;
  };

  const updateEdge = () => {
    if (!presentation.active || presentation.following || !presentation.target) {
      edge.hidden = true;
      return;
    }
    const target = (presentation.target.row_id && document.getElementById(presentation.target.row_id))
      || (presentation.target.region_id && document.querySelector(`[data-region="${CSS.escape(presentation.target.region_id)}"]`));
    if (!target) return;
    const rect = target.getBoundingClientRect();
    edge.textContent = rect.bottom < 72 ? "Presenter ↑" : rect.top > innerHeight ? "Presenter ↓" : "Presenter here";
    edge.hidden = false;
  };

  const pauseFollow = () => {
    if (!presentation.active || !presentation.following) return;
    presentation.following = false;
    presenterStatus.hidden = true;
    rejoin.hidden = false;
    updateEdge();
  };

  const moveToTarget = async () => {
    if (!presentation.target) return;
    clearPresenterTarget();
    const target = await materializeTarget(presentation.target);
    if (!target || !presentation.following) return;
    presenterRows = targetRows(presentation.target);
    presenterRows.forEach((row) => row.classList.add(reducedMotion.matches ? "present-target-static" : "present-target"));
    suppressScroll = true;
    target.scrollIntoView({ behavior: reducedMotion.matches ? "auto" : "smooth", block: "center" });
    const release = () => { suppressScroll = false; };
    if ("onscrollend" in window) addEventListener("scrollend", release, { once: true });
    else setTimeout(release, reducedMotion.matches ? 0 : 900);
    edge.hidden = true;
  };

  const rejoinFollow = () => {
    if (!presentation.active) return;
    presentation.following = true;
    presenterStatus.hidden = false;
    rejoin.hidden = true;
    moveToTarget();
  };
  rejoin?.addEventListener("click", rejoinFollow);
  edge?.addEventListener("click", rejoinFollow);

  const applyPresent = (update) => {
    const status = update.status || { active: false };
    if (update.command === "pending") {
      presenter.hidden = false;
      presenterStatus.hidden = false;
      presenterStatus.textContent = "Presenter waiting";
      note.textContent = update.note || "Presenter is waiting for the current edit to finish.";
      note.hidden = false;
      return;
    }
    if (update.command === "end" || !status.active && update.command !== "focus") {
      clearPresenterTarget();
      note.hidden = true;
      note.textContent = "";
      presenter.hidden = true;
      if (presentation.ownsMode && presentation.priorMode) setMode(presentation.priorMode);
      presentation = { active: false, following: false, target: null, ownsMode: false, priorMode: null };
      return;
    }
    if ((status.active || update.command === "focus") && !presentation.active) {
      presentation.priorMode = body.dataset.mode;
      presentation.ownsMode = status.active && body.dataset.mode !== "guided";
      if (presentation.ownsMode) setMode("guided");
      presentation.following = true;
    }
    presentation.active = status.active || update.command === "focus" || presentation.active;
    presentation.target = update.target || (update.command === "focus" ? presentation.target : null);
    presenter.hidden = false;
    presenterStatus.hidden = !presentation.following;
    rejoin.hidden = presentation.following;
    presenterStatus.textContent = status.active
      ? `Following presenter · ${Number(status.slide_index) + 1}/${status.slide_count}`
      : "Presenter focus";
    if (typeof update.note === "string" && update.note.trim()) {
      note.textContent = update.note;
      note.hidden = false;
    } else {
      note.hidden = true;
    }
    if (presentation.following) moveToTarget();
    else updateEdge();
  };

  ["wheel", "touchstart"].forEach((name) => addEventListener(name, pauseFollow, { passive: true }));
  addEventListener("scroll", () => {
    if (presentation.active && !suppressScroll) pauseFollow();
    updateEdge();
    reportInteraction(search === document.activeElement ? "search" : null);
  }, { passive: true });
  addEventListener("keydown", (event) => {
    if (presentation.active && !presentation.following && event.key.toLowerCase() === "r" && !event.target.closest("input,textarea,[contenteditable=true]")) {
      event.preventDefault();
      rejoinFollow();
    }
  });

  const source = new EventSource(`/events?${new URLSearchParams({ token, generation: String(generation), tab: tabId })}`);
  source.addEventListener("state", (message) => {
    try {
      const update = JSON.parse(message.data);
      if (!Number.isSafeInteger(update.generation) || update.generation <= generation) return;
      if (!update.full && update.generation !== generation + 1) return reload();
      const activeId = document.activeElement?.id;
      const editorDrafts = [...document.querySelectorAll(".reply-form,.comment-edit-form")].map((form) => ({
        id: form.dataset.commentId,
        edit: form.classList.contains("comment-edit-form"),
        body: form.querySelector("textarea")?.value || "",
      })).filter((draft) => draft.body);
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
      returnedGeneration = Math.max(returnedGeneration, generation);
      body.dataset.generation = String(generation);
      for (const draft of editorDrafts) {
        const selector = draft.edit ? `.comment-edit-form[data-comment-id="${CSS.escape(draft.id)}"]` : `.reply-form[data-comment-id="${CSS.escape(draft.id)}"]`;
        let form = document.querySelector(selector);
        if (!form && draft.edit) {
          document.querySelector(`button[data-action="comment-edit"][data-comment-id="${CSS.escape(draft.id)}"]`)?.click();
          form = document.querySelector(selector);
        }
        const textarea = form?.querySelector("textarea");
        if (textarea) textarea.value = draft.body;
      }
      if (activeId) document.getElementById(activeId)?.focus({ preventScroll: true });
      if (anchorId && Number.isFinite(anchorTop)) {
        const moved = document.querySelector(`[data-region="${CSS.escape(anchorId)}"]`);
        if (moved) scrollBy(0, moved.getBoundingClientRect().top - anchorTop);
      }
      if (presentation.target) presentation.following ? moveToTarget() : updateEdge();
      observeSkeletons();
    } catch (_) {
      reload();
    }
  });
  source.addEventListener("present", (message) => {
    try {
      // Rendering is scheduled once per frame and only the newest watch-channel
      // value survives server/client bursts.
      window.ganderPendingPresent = JSON.parse(message.data);
      if (!window.ganderPresentFrame) window.ganderPresentFrame = requestAnimationFrame(() => {
        window.ganderPresentFrame = 0;
        applyPresent(window.ganderPendingPresent);
      });
    } catch (_) {}
  });

  reportInteraction(null);
})();
