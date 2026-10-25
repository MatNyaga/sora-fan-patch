// Fan-patch installer UI. One screen at a time: welcome → working → done | problem.
// A pack release (release.json format 3, P2.11) adds the fighter checklist to the welcome screen:
// every fighter is ticked at the start, the titles name the ticked fighters, and the build gets
// the ticked pack ids. A one-patch release (formats 1 and 2) shows no checklist (as in 1.0).
// Runs inside the Tauri app (window.__TAURI__) or, in a plain browser, as a clickable demo
// with simulated progress (that is the review mock: mock/index.html).
(() => {
  "use strict";
  const S = window.SORA_STRINGS;
  const T = window.__TAURI__;
  const q = new URLSearchParams(location.search);
  const $ = (id) => document.getElementById(id);

  const t = (key, p = {}) => {
    let s = S[key] ?? key;
    for (const [k, v] of Object.entries(p)) {
      // Disc codes like ULUS-10566 must not break across lines: use a non-breaking hyphen.
      const val = /^[A-Z]{4}-\d{5}$/.test(String(v)) ? String(v).replace("-", "\u2011") : String(v);
      s = s.split(`{${k}}`).join(val);
    }
    return s;
  };
  document.querySelectorAll("[data-t]").forEach((el) => (el.textContent = t(el.dataset.t)));
  document.querySelectorAll("[data-aria]").forEach((el) => {
    el.setAttribute("aria-label", t(el.dataset.aria));
    el.title = t(el.dataset.aria);
  });

  // ------------------------------------------------------------------ backends
  const tauriBackend = () => ({
    info: () => T.core.invoke("release_info"),
    start: (path, outDir, packs) => T.core.invoke("start", { path, outDir: outDir ?? null, packs: packs ?? null }),
    cancel: () => T.core.invoke("cancel"),
    chooseFile: () =>
      T.dialog.open({ multiple: false, directory: false, filters: [{ name: t("filter_name"), extensions: ["iso", "ISO"] }] }),
    chooseFolder: () => T.dialog.open({ multiple: false, directory: true }),
    reveal: (p) => T.core.invoke("reveal", { path: p }),
    close: () => T.window.getCurrentWindow().close(),
    onProgress: (cb) => T.event.listen("progress", (e) => cb(e.payload)),
    onOutcome: (cb) => T.event.listen("outcome", (e) => cb(e.payload)),
    onDrag: (cb) => T.webviewWindow.getCurrentWebviewWindow().onDragDropEvent((e) => cb(e.payload)),
    autotest: () => T.core.invoke("autotest"),
  });

  // Mock fighter lists: ?packs=0 = one-patch release (1.0 look), default 1 = Sora, 2 = Sora + an example.
  const DEMO_PACKS = [
    { id: "sora", name: "Sora", blurb: "Fan-ported from Kingdom Hearts II: own moves, EX Mode, EX Burst and Reflect." },
    { id: "example", name: "Example Fighter", blurb: "(mock only) A second fighter pack, to show the list." },
  ];
  const demoInfo = () => {
    const n = Number(q.get("packs") ?? 1);
    if (!n) return { title: S.app_title, version: "1.0", output_name: "Dissidia 012 + Sora (fan patch 1.0).iso", packs: [], sets: [] };
    const packs = DEMO_PACKS.slice(0, n);
    const sets = n === 1 ? [["sora"]] : [["sora"], ["example"], ["sora", "example"]];
    return { title: S.app_title, version: "1.1", packs,
      sets: sets.map((p) => ({ packs: p, file_name: `Dissidia 012 + ${p.map((i) => packs.find((x) => x.id === i).name).join(" + ")} (fan patch 1.1).iso` })) };
  };
  const demoBackend = () => {
    let prog = () => {}, outcome = () => {}, timer = null;
    const demoFolder = "C:\\Users\\Player\\Games";
    return {
      info: async () => demoInfo(),
      start: async (path, outDir, packs) => {
        const plan = [["checking", 2600], ["building", 4200], ["verifying", 700]];
        let i = 0, t0 = performance.now();
        clearInterval(timer);
        timer = setInterval(() => {
          const [stage, ms] = plan[i];
          const f = Math.min(1, (performance.now() - t0) / ms);
          prog({ stage, done: Math.round(f * 1000), total: 1000 });
          if (f >= 1) {
            i++; t0 = performance.now();
            if (i === plan.length) {
              clearInterval(timer);
              if (/europe|eu\b|ules/i.test(path))
                outcome({ kind: "problem", problem: demoProblem("wrong_region") });
              else {
                const set = demoInfo().sets.find((x) => sameSet(x.packs, packs ?? []));
                const file = set ? set.file_name : "Dissidia 012 + Sora (fan patch 1.0).iso";
                outcome({ kind: "done", path: demoFolder + "\\" + file, file, folder: demoFolder });
              }
            }
          }
        }, 50);
      },
      cancel: async () => { clearInterval(timer); outcome({ kind: "problem", problem: { code: "cancelled", params: {}, detail: "" } }); },
      chooseFile: async () => "C:\\Users\\Player\\Games\\Dissidia 012 (USA).iso",
      chooseFolder: async () => "C:\\Users\\Player\\Documents",
      reveal: async () => alert("(demo) This opens the folder in Explorer / Finder / Files."),
      close: async () => alert("(demo) This closes the window."),
      onProgress: (cb) => (prog = cb),
      onOutcome: (cb) => (outcome = cb),
      onDrag: () => {},
      autotest: async () => null,
    };
  };

  // Sample values for the mock's error screens.
  const DEMO_PARAMS = {
    archive: { kind: "ZIP" }, compressed_image: { kind: "CISO" }, other_game: { id: "ULUS-10391" },
    wrong_region: { region: "European", id: "ULES-01505", want: "ULUS-10566" },
    incomplete: { have: "1.1 GB", want: "1.8 GB" }, already_patched: { version: "1.0" }, older_patch: { version: "0.9" },
    already_has: { version: "1.1", names: "Sora" }, older_has: { version: "1.0", names: "Sora" }, pack_mix: { names: "Example Fighter" },
    no_space: { need: "1.9 GB", have: "0.8 GB" }, build_failed: { code: "E-HASH" },
  };
  const demoProblem = (code) => ({ code, params: DEMO_PARAMS[code] ?? {}, detail: code === "build_failed" ? "size 1812201472 sha256 3f9c…" : "" });

  const api = T ? tauriBackend() : demoBackend();

  // ------------------------------------------------------------------ fighter checklist (pack release)
  const sameSet = (a, b) => a.length === b.length && [...a].sort().join("+") === [...b].sort().join("+");
  // Plain-English list, same rule as the core: "Sora", "Sora and X", "Sora, X and Y".
  const joinNames = (n) => (n.length < 2 ? n.join("") : n.slice(0, -1).join(", ") + " and " + n[n.length - 1]);
  let rel = { packs: [], sets: [] };
  const isPacks = () => rel.packs.length > 0;
  const chosen = () => [...document.querySelectorAll("#pack-list input")].filter((i) => i.checked).map((i) => i.value);
  const chosenNames = () => joinNames(chosen().map((id) => rel.packs.find((p) => p.id === id).name));
  // "" = can build; else the strings key of the reason why not.
  const choiceProblem = () => {
    if (!isPacks()) return "";
    const c = chosen();
    if (!c.length) return "packs_none";
    return rel.sets.some((s) => sameSet(s.packs, c)) ? "" : "packs_mix";
  };

  function renderPacks() {
    if (!isPacks()) return;
    const list = $("pack-list");
    list.textContent = "";
    for (const p of rel.packs) {
      const row = document.createElement("label");
      row.className = "pack on";
      const box = document.createElement("input");
      box.type = "checkbox";
      box.value = p.id;
      box.checked = true;
      const name = document.createElement("span");
      name.className = "pack-name";
      name.textContent = p.name;
      const blurb = document.createElement("span");
      blurb.className = "pack-blurb";
      blurb.textContent = p.blurb || "";
      row.append(box, name, blurb);
      box.addEventListener("change", () => { row.classList.toggle("on", box.checked); updateChoice(); });
      list.appendChild(row);
    }
    $("packs").hidden = false;
    $("step-build").textContent = t("step_build_packs");
    updateChoice();
  }

  function updateChoice() {
    if (!isPacks()) return;
    const c = chosen();
    $("welcome-title").textContent = c.length ? t("welcome_title_packs", { names: chosenNames() }) : t("welcome_title_none");
    $("welcome-body").textContent = t("welcome_body_packs");
    const why = choiceProblem();
    $("packs-warn").hidden = !why;
    $("packs-warn").textContent = why ? t(why) : "";
    $("drop").setAttribute("aria-disabled", why ? "true" : "false");
  }

  // ------------------------------------------------------------------ state
  let current = { path: null, outDir: null, stage: null };

  function show(name) {
    document.querySelectorAll(".screen").forEach((s) => s.classList.toggle("on", s.dataset.screen === name));
    const h = document.querySelector(`.screen[data-screen="${name}"] h1`);
    if (h && !document.body.classList.contains("embed")) h.focus({ preventScroll: true });
  }
  const say = (msg) => { $("live").textContent = msg; };
  const baseName = (p) => p.split(/[\\/]/).pop();

  function begin(path, outDir = null) {
    if (choiceProblem()) {
      show("welcome");
      updateChoice();
      $("pack-list").querySelector("input")?.focus();
      return;
    }
    current = { path, outDir, stage: null, packs: isPacks() ? chosen() : null, names: isPacks() ? chosenNames() : "" };
    $("work-file").textContent = baseName(path);
    setStage("checking", 0, 1);
    show("working");
    api.start(path, outDir, current.packs).catch((e) => onOutcome({ kind: "problem", problem: { code: "build_failed", params: { code: "E-APP" }, detail: String(e) } }));
  }

  const ORDER = ["checking", "building", "verifying"];
  const STAGE_TEXT = {
    checking: ["checking_title", "checking_body"],
    building: ["building_title", "building_body"],
    verifying: ["verifying_title", "verifying_body"],
  };
  function setStage(stage, done, total) {
    const pct = total ? Math.floor((done / total) * 100) : 0;
    if (stage !== current.stage) {
      current.stage = stage;
      const titleKey = stage === "building" && current.packs ? "building_title_packs" : STAGE_TEXT[stage][0];
      $("work-title").textContent = t(titleKey, { names: current.names });
      $("work-body").textContent = t(STAGE_TEXT[stage][1]);
      say(t(titleKey, { names: current.names }));
      const n = ORDER.indexOf(stage);
      document.querySelectorAll(".steps li").forEach((li) => {
        const k = ORDER.indexOf(li.dataset.step);
        li.classList.toggle("done", k < n);
        li.classList.toggle("now", k === n);
        if (k === n) li.setAttribute("aria-current", "step"); else li.removeAttribute("aria-current");
      });
    }
    $("bar-fill").style.width = pct + "%";
    $("bar").setAttribute("aria-valuenow", String(pct));
    $("bar").setAttribute("aria-label", t(STAGE_TEXT[stage][0]));
    $("pct").textContent = t("percent", { n: pct });
  }

  function onOutcome(o) {
    if (o.kind === "done") {
      if (current.packs) {
        const many = current.packs.length > 1;
        $("done-title").textContent = t(many ? "done_title_many" : "done_title_one", { names: current.names });
      }
      $("done-file").textContent = o.file;
      $("done-folder").textContent = o.folder;
      $("open-folder").onclick = () => api.reveal(o.path);
      show("done");
      say($("done-title").textContent);
      return;
    }
    const p = o.problem;
    const good = p.code === "already_patched" || p.code === "already_has";
    const stopped = p.code === "cancelled";
    const badge = $("problem-badge");
    badge.className = "badge" + (good ? " good" : stopped ? " stop" : "");
    badge.textContent = good ? "★" : stopped ? "■" : "!";
    $("problem-title").textContent = t(`err_${p.code}_title`, p.params);
    $("problem-body").textContent = t(`err_${p.code}_body`, p.params);
    const folderFix = p.code === "no_space" || p.code === "cant_write";
    $("problem-folder").hidden = !folderFix;
    $("problem-again").className = "btn " + (folderFix ? "ghost" : "primary");
    $("problem-details").hidden = good || stopped;
    $("problem-code").textContent = [p.code, ...Object.entries(p.params).map(([k, v]) => `${k}=${v}`), p.detail].filter(Boolean).join("\n");
    show("problem");
    say($("problem-title").textContent);
  }

  // ------------------------------------------------------------------ wiring
  async function chooseFile() {
    if (choiceProblem()) return begin(null);
    const p = await api.chooseFile();
    if (p) begin(typeof p === "string" ? p : p.path);
  }
  $("drop").addEventListener("click", chooseFile);
  $("problem-again").addEventListener("click", () => show("welcome"));
  $("done-close").addEventListener("click", () => api.close());
  $("cancel").addEventListener("click", () => api.cancel());
  $("problem-folder").addEventListener("click", async () => {
    const d = await api.chooseFolder();
    if (d && current.path) begin(current.path, d);
  });
  $("help-open").addEventListener("click", () => $("help").showModal());

  // Text size: A− / A+ and Ctrl/Cmd + = / - / 0. Remembered on this computer if possible.
  const SCALES = [1, 1.15, 1.3, 1.5];
  let scale = 0;
  try { scale = Math.max(0, SCALES.indexOf(Number(localStorage.getItem("textScale")))); } catch (_) {}
  const applyScale = () => {
    document.documentElement.style.setProperty("--scale", SCALES[scale]);
    try { localStorage.setItem("textScale", String(SCALES[scale])); } catch (_) {}
  };
  applyScale();
  $("text-bigger").addEventListener("click", () => { scale = Math.min(SCALES.length - 1, scale + 1); applyScale(); });
  $("text-smaller").addEventListener("click", () => { scale = Math.max(0, scale - 1); applyScale(); });
  document.addEventListener("keydown", (e) => {
    const mod = e.ctrlKey || e.metaKey;
    if (mod && (e.key === "=" || e.key === "+")) { e.preventDefault(); $("text-bigger").click(); }
    else if (mod && e.key === "-") { e.preventDefault(); $("text-smaller").click(); }
    else if (mod && e.key === "0") { e.preventDefault(); scale = 0; applyScale(); }
    else if (mod && e.key.toLowerCase() === "o") {
      const on = document.querySelector(".screen.on")?.dataset.screen;
      if (on === "welcome" || on === "problem" || on === "done") { e.preventDefault(); chooseFile(); }
    } else if (e.key === "F1") { e.preventDefault(); $("help").showModal(); }
  });

  // Drag and drop: the OS gives the app real file paths (Tauri); the demo uses HTML5 events.
  const dropAllowed = () => ["welcome", "problem", "done"].includes(document.querySelector(".screen.on")?.dataset.screen);
  api.onDrag((d) => {
    if (d.type === "enter" || d.type === "over") {
      if (dropAllowed()) { document.body.classList.add("dragging"); if (document.querySelector(".screen.on").dataset.screen !== "welcome") show("welcome"); }
    } else if (d.type === "leave") document.body.classList.remove("dragging");
    else if (d.type === "drop") {
      document.body.classList.remove("dragging");
      if (dropAllowed() && d.paths && d.paths.length) begin(d.paths[0]);
    }
  });
  if (!T) {
    document.addEventListener("dragover", (e) => { e.preventDefault(); if (dropAllowed()) document.body.classList.add("dragging"); });
    document.addEventListener("dragleave", (e) => { if (!e.relatedTarget) document.body.classList.remove("dragging"); });
    document.addEventListener("drop", (e) => {
      e.preventDefault(); document.body.classList.remove("dragging");
      const f = e.dataTransfer.files[0];
      if (f && dropAllowed()) begin(f.name);
    });
  }

  api.onProgress((p) => setStage(p.stage, p.done, p.total));
  api.onOutcome(onOutcome);
  const ready = api.info().then((i) => {
    $("ver").textContent = t("app_version", { version: i.version });
    rel = { packs: i.packs || [], sets: i.sets || [] };
    renderPacks();
  });

  // ------------------------------------------------------------------ start
  if (!T) {
    // Mock: ?screen=welcome|dragging|checking|building|verifying|done|err:<code>  &embed=1 hides the mock bar.
    if (q.get("embed")) document.body.classList.add("embed");
    const goto = (s) => {
      document.body.classList.toggle("dragging", s === "dragging");
      if (s === "welcome" || s === "dragging") return show("welcome");
      if (ORDER.includes(s)) {
        current.stage = null;
        if (isPacks()) current = { ...current, packs: chosen(), names: chosenNames() };
        $("work-file").textContent = "Dissidia 012 (USA).iso";
        setStage(s, { checking: 420, building: 630, verifying: 900 }[s], 1000);
        return show("working");
      }
      if (s === "done") {
        if (isPacks()) current = { ...current, packs: chosen(), names: chosenNames() };
        const set = rel.sets.find((x) => sameSet(x.packs, current.packs ?? []));
        return onOutcome({ kind: "done", path: "", file: set ? set.file_name : "Dissidia 012 + Sora (fan patch 1.0).iso", folder: "C:\\Users\\Player\\Games" });
      }
      if (s.startsWith("err:")) return onOutcome({ kind: "problem", problem: demoProblem(s.slice(4)) });
    };
    ready.then(() => goto(q.get("screen") || "welcome"));
    if (q.get("help")) $("help").showModal();
    const bar = document.createElement("div");
    bar.className = "mockbar";
    bar.innerHTML = "<span style='color:#fff;opacity:.7;align-self:center'>mock:</span>";
    ["welcome", "dragging", "checking", "building", "done", "err:wrong_region", "err:already_patched", "err:already_has", "err:older_has", "err:no_space", "err:build_failed", "err:cancelled"].forEach((s) => {
      const b = document.createElement("button");
      b.textContent = s.replace("err:", "");
      b.onclick = () => goto(s);
      bar.appendChild(b);
    });
    document.body.appendChild(bar);
  } else {
    show("welcome");
    // Test hook: SORAPATCH_AUTOTEST_PACKS=a,b ticks exactly those fighters before starting.
    ready.then(() => api.autotest()).then((a) => {
      if (!a) return;
      if (a.packs && isPacks()) {
        document.querySelectorAll("#pack-list input").forEach((i) => { i.checked = a.packs.includes(i.value); i.closest(".pack").classList.toggle("on", i.checked); });
        updateChoice();
      }
      begin(a.input, a.out_dir);
    });
  }
})();
