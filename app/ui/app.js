// Fan-patch installer UI. One screen at a time: welcome → working → done | problem.
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
    start: (path, outDir) => T.core.invoke("start", { path, outDir: outDir ?? null }),
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

  const demoBackend = () => {
    let prog = () => {}, outcome = () => {}, timer = null;
    const demoFolder = "C:\\Users\\Player\\Games";
    return {
      info: async () => ({ title: S.app_title, version: "1.0", output_name: "Dissidia 012 + Sora (fan patch 1.0).iso" }),
      start: async (path) => {
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
              else outcome({ kind: "done", path: demoFolder + "\\Dissidia 012 + Sora (fan patch 1.0).iso", file: "Dissidia 012 + Sora (fan patch 1.0).iso", folder: demoFolder });
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
    no_space: { need: "1.9 GB", have: "0.8 GB" }, build_failed: { code: "E-HASH" },
  };
  const demoProblem = (code) => ({ code, params: DEMO_PARAMS[code] ?? {}, detail: code === "build_failed" ? "size 1812201472 sha256 3f9c…" : "" });

  const api = T ? tauriBackend() : demoBackend();

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
    current = { path, outDir, stage: null };
    $("work-file").textContent = baseName(path);
    setStage("checking", 0, 1);
    show("working");
    api.start(path, outDir).catch((e) => onOutcome({ kind: "problem", problem: { code: "build_failed", params: { code: "E-APP" }, detail: String(e) } }));
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
      $("work-title").textContent = t(STAGE_TEXT[stage][0]);
      $("work-body").textContent = t(STAGE_TEXT[stage][1]);
      say(t(STAGE_TEXT[stage][0]));
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
      $("done-file").textContent = o.file;
      $("done-folder").textContent = o.folder;
      $("open-folder").onclick = () => api.reveal(o.path);
      show("done");
      say(t("done_title"));
      return;
    }
    const p = o.problem;
    const good = p.code === "already_patched";
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
  api.info().then((i) => { $("ver").textContent = t("app_version", { version: i.version }); });

  // ------------------------------------------------------------------ start
  if (!T) {
    // Mock: ?screen=welcome|dragging|checking|building|verifying|done|err:<code>  &embed=1 hides the mock bar.
    if (q.get("embed")) document.body.classList.add("embed");
    const goto = (s) => {
      document.body.classList.toggle("dragging", s === "dragging");
      if (s === "welcome" || s === "dragging") return show("welcome");
      if (ORDER.includes(s)) {
        current.stage = null;
        $("work-file").textContent = "Dissidia 012 (USA).iso";
        setStage(s, { checking: 420, building: 630, verifying: 900 }[s], 1000);
        return show("working");
      }
      if (s === "done") return onOutcome({ kind: "done", path: "", file: "Dissidia 012 + Sora (fan patch 1.0).iso", folder: "C:\\Users\\Player\\Games" });
      if (s.startsWith("err:")) return onOutcome({ kind: "problem", problem: demoProblem(s.slice(4)) });
    };
    goto(q.get("screen") || "welcome");
    if (q.get("help")) $("help").showModal();
    const bar = document.createElement("div");
    bar.className = "mockbar";
    bar.innerHTML = "<span style='color:#fff;opacity:.7;align-self:center'>mock:</span>";
    ["welcome", "dragging", "checking", "building", "done", "err:wrong_region", "err:already_patched", "err:no_space", "err:build_failed", "err:cancelled"].forEach((s) => {
      const b = document.createElement("button");
      b.textContent = s.replace("err:", "");
      b.onclick = () => goto(s);
      bar.appendChild(b);
    });
    document.body.appendChild(bar);
  } else {
    show("welcome");
    api.autotest().then((a) => { if (a) begin(a.input, a.out_dir); });
  }
})();
