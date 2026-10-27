// AutoTappo 前端主逻辑 (支持原生浏览器独立双击预览 & Gitee国内直连检测 & 可自定义全局热键)

(function () {
  "use strict";

  const CURRENT_VERSION = "v0.2.0"; // TEST-ONLY: 伪装旧版本，用于实测一键更新
  const GITEE_API_URL = "https://gitee.com/api/v5/repos/To0Ru/AutoTappo/releases/latest";
  const GITHUB_API_URL = "https://api.github.com/repos/To0Ru/AutoTappo/releases/latest";

  // 兜底源：仓库内置的 version.json。
  // Release API 在部分网络下会 404（Gitee 未发版）或被限流（GitHub 未认证 403），
  // 而 raw 文件读取不受这些限制，所以作为最后一道保险。
  const VERSION_FILE_URLS = [
    "https://gitee.com/To0Ru/AutoTappo/raw/main/version.json",
    "https://raw.githubusercontent.com/To0Ru/AutoTappo/main/version.json",
    "https://cdn.jsdelivr.net/gh/To0Ru/AutoTappo@main/version.json",
  ];
  const UPDATE_FETCH_TIMEOUT_MS = 6000;

  // 检测是否在 Tauri 原生客户端运行
  const isTauri = typeof window.__TAURI__ !== "undefined" && typeof window.__TAURI__.core !== "undefined";

  // 安全 invoke 包装
  const invoke = isTauri
    ? window.__TAURI__.core.invoke
    : async (cmd, args) => {
        console.log("[Browser Preview Mode] Invoke called:", cmd, args);
        return null;
      };

  // DOM 元素引用
  const statusBadge = document.getElementById("status-badge");
  const counterValue = document.getElementById("counter-value");
  const btnToggle = document.getElementById("btn-toggle");
  const btnToggleIcon = document.getElementById("btn-toggle-icon");
  const btnToggleText = document.getElementById("btn-toggle-text");
  const btnReset = document.getElementById("btn-reset");
  const speedButtons = document.querySelectorAll(".speed-btn");
  const btnBell = document.getElementById("btn-bell");
  const updateDot = document.getElementById("update-dot");
  const btnHide = document.getElementById("btn-hide");
  const btnQuit = document.getElementById("btn-quit");

  // 热键自定义 DOM
  const btnHotkey = document.getElementById("btn-hotkey");
  const lblHotkey = document.getElementById("lbl-hotkey");
  const hotkeyTip = document.getElementById("hotkey-tip");

  // Modal 弹窗元素
  const updateModal = document.getElementById("update-modal");
  const btnModalClose = document.getElementById("btn-modal-close");
  const lblCurVersion = document.getElementById("lbl-cur-version");
  const lblLatestVersion = document.getElementById("lbl-latest-version");
  const updateStatusBox = document.getElementById("update-status-box");
  const updateStatusText = document.getElementById("update-status-text");
  const updateSpinner = document.getElementById("update-spinner");
  const changelogArea = document.getElementById("changelog-area");
  const changelogContent = document.getElementById("changelog-content");
  const updateGuideCard = document.getElementById("update-guide-card");
  const linkGithubRepo = document.getElementById("link-github-repo");
  const lblGithubRepoUrl = document.getElementById("lbl-github-repo-url");
  const linkGiteeRepo = document.getElementById("link-gitee-repo");
  const lblGiteeRepoUrl = document.getElementById("lbl-gitee-repo-url");
  const btnCheckNow = document.getElementById("btn-check-now");
  const btnCheckText = document.getElementById("btn-check-text");
  const btnDownload = document.getElementById("btn-download");
  const btnGitee = document.getElementById("btn-gitee");
  const updateProgressBox = document.getElementById("update-progress");
  const updateProgressFill = document.getElementById("update-progress-fill");
  const updateProgressText = document.getElementById("update-progress-text");
  const btnUpdateCancel = document.getElementById("btn-update-cancel");

  // 两个仓库地址（发版页），供指引卡片与底部按钮共用
  const GITHUB_RELEASES_URL = "https://github.com/To0Ru/AutoTappo/releases";
  const GITEE_RELEASES_URL = "https://gitee.com/To0Ru/AutoTappo/releases";

  // 状态变量
  let currentRunning = false;
  let currentSpeed = 0;
  let localSentCount = 0;
  let previewInterval = null;
  let lastCheckTime = 0;
  let cachedLatestData = null;
  // 各通道原始 Release 数据（用于挑出真实资产下载地址）
  let cachedCandidates = [];
  let updateBusy = false;

  // 热键状态 (默认 F6, VK: 0x75 = 117)
  let currentHotkeyName = localStorage.getItem("autotappo_hotkey_name") || "F6";
  let currentHotkeyVK = parseInt(localStorage.getItem("autotappo_hotkey_vk"), 10) || 0x75;
  let isRecordingHotkey = false;

  const versionText = document.querySelector(".version-text");
  if (versionText) versionText.textContent = CURRENT_VERSION;
  if (lblCurVersion) lblCurVersion.textContent = CURRENT_VERSION;
  if (lblHotkey) lblHotkey.textContent = currentHotkeyName;

  // 更新主界面 UI
  function updateUI(status) {
    if (!status) return;

    currentRunning = status.running;
    currentSpeed = status.speed_mode;
    localSentCount = status.total_sent;

    // 1. 运行状态徽章
    if (statusBadge) {
      if (currentRunning) {
        statusBadge.textContent = "运行中";
        statusBadge.classList.add("running");
      } else {
        statusBadge.textContent = "已暂停";
        statusBadge.classList.remove("running");
      }
    }

    // 2. 模拟点击计数
    if (counterValue) {
      counterValue.textContent = localSentCount.toLocaleString();
    }

    // 3. 大按钮状态
    if (btnToggle && btnToggleText && btnToggleIcon) {
      if (currentRunning) {
        btnToggle.classList.add("running");
        btnToggleIcon.textContent = "⏸";
        btnToggleText.textContent = `暂停挂机 (${currentHotkeyName})`;
      } else {
        btnToggle.classList.remove("running");
        btnToggleIcon.textContent = "▶";
        btnToggleText.textContent = `开始挂机 (${currentHotkeyName})`;
      }
    }

    // 4. 速度档位高亮
    speedButtons.forEach((btn) => {
      const sp = parseInt(btn.dataset.speed, 10);
      if (sp === currentSpeed) {
        btn.classList.add("active");
      } else {
        btn.classList.remove("active");
      }
    });
  }

  // 轮询同步状态
  async function pollStatus() {
    if (isTauri) {
      try {
        const status = await invoke("get_status");
        updateUI(status);
      } catch (err) {
        console.error("Failed to fetch status:", err);
      }
    }
  }

  // ---------------- 浏览器独立预览模式支持 ----------------
  if (!isTauri) {
    console.info("💡 当前处于【纯浏览器预览模式】，已自动开启交互模拟与测试。");
    updateUI({
      running: false,
      speed_mode: 0,
      total_sent: 0,
    });

    function startPreviewLoop() {
      if (previewInterval) clearInterval(previewInterval);
      const speedIntervals = [100, 45, 22]; // 休闲(~10/s), 快速(~22/s), 极速(~45/s)
      previewInterval = setInterval(() => {
        if (currentRunning) {
          localSentCount += 1;
          if (counterValue) counterValue.textContent = localSentCount.toLocaleString();
        }
      }, speedIntervals[currentSpeed] || 100);
    }
    startPreviewLoop();
  }

  // ---------------- 交互事件绑定 ----------------
  if (btnToggle) {
    btnToggle.addEventListener("click", async () => {
      if (isTauri) {
        try {
          const newRunning = await invoke("toggle_running");
          currentRunning = newRunning;
          await pollStatus();
        } catch (err) {
          console.error("Failed to toggle running:", err);
        }
      } else {
        currentRunning = !currentRunning;
        updateUI({
          running: currentRunning,
          speed_mode: currentSpeed,
          total_sent: localSentCount,
        });
      }
    });
  }

  if (btnReset) {
    btnReset.addEventListener("click", async () => {
      if (isTauri) {
        try {
          await invoke("reset_sent_count");
        } catch (err) {
          console.error("Failed to reset count:", err);
        }
      }
      localSentCount = 0;
      if (counterValue) counterValue.textContent = "0";
    });
  }

  speedButtons.forEach((btn) => {
    btn.addEventListener("click", async () => {
      const mode = parseInt(btn.dataset.speed, 10);
      currentSpeed = mode;
      if (isTauri) {
        try {
          await invoke("set_speed", { mode });
          await pollStatus();
        } catch (err) {
          console.error("Failed to set speed:", err);
        }
      } else {
        updateUI({
          running: currentRunning,
          speed_mode: currentSpeed,
          total_sent: localSentCount,
        });
      }
    });
  });

  const hideWindow = async () => {
    if (isTauri) {
      try {
        await invoke("minimize_to_tray");
      } catch (err) {
        console.error("Failed to minimize:", err);
      }
    } else {
      alert("【预览提示】在编译后的应用中，点击将无缝收进 Windows 系统托盘守护。");
    }
  };

  if (btnHide) btnHide.addEventListener("click", hideWindow);

  if (btnQuit) {
    btnQuit.addEventListener("click", async () => {
      if (isTauri) {
        try {
          await invoke("quit_app");
        } catch (err) {
          console.error("Failed to quit app:", err);
        }
      } else {
        alert("【预览提示】在编译后的应用中，点击将完全退出守护进程。");
      }
    });
  }

  // ---------------- 全局热键自定义录制逻辑 ----------------
  const FORBIDDEN_HOTKEYS = {
    F7: "F7 为官方游戏模式键，无法绑定",
    F8: "F8 为官方多屏幕切换键，无法绑定",
    F9: "F9 为官方老板键隐藏键，无法绑定",
  };

  function getWinVirtualKey(e) {
    if (e.code.startsWith("F") && !isNaN(e.code.slice(1))) {
      const num = parseInt(e.code.slice(1), 10);
      if (num >= 1 && num <= 12) return { vk: 0x6f + num, name: e.code };
    }
    const specialMap = {
      Home: { vk: 0x24, name: "Home" },
      End: { vk: 0x23, name: "End" },
      PageUp: { vk: 0x21, name: "PgUp" },
      PageDown: { vk: 0x22, name: "PgDn" },
      Insert: { vk: 0x2d, name: "Ins" },
      Delete: { vk: 0x2e, name: "Del" },
      Pause: { vk: 0x13, name: "Pause" },
      ScrollLock: { vk: 0x91, name: "ScrLk" },
      Backquote: { vk: 0xc0, name: "`" },
      NumpadAdd: { vk: 0x6b, name: "Num+" },
      NumpadSubtract: { vk: 0x6d, name: "Num-" },
      NumpadMultiply: { vk: 0x6a, name: "Num*" },
      NumpadDivide: { vk: 0x6f, name: "Num/" },
    };
    if (specialMap[e.code]) return specialMap[e.code];

    if (e.code.startsWith("Key")) {
      const char = e.code.slice(3).toUpperCase();
      return { vk: char.charCodeAt(0), name: char };
    }

    return null;
  }

  if (btnHotkey) {
    btnHotkey.addEventListener("click", () => {
      if (isRecordingHotkey) {
        cancelRecording();
        return;
      }
      startRecording();
    });
  }

  function startRecording() {
    isRecordingHotkey = true;
    if (btnHotkey) btnHotkey.classList.add("recording");
    if (lblHotkey) lblHotkey.textContent = "请按键...";
    if (hotkeyTip) hotkeyTip.textContent = "按 Esc 取消";

    window.addEventListener("keydown", handleRecordKey, { capture: true });
  }

  function cancelRecording() {
    isRecordingHotkey = false;
    if (btnHotkey) btnHotkey.classList.remove("recording");
    if (lblHotkey) lblHotkey.textContent = currentHotkeyName;
    if (hotkeyTip) hotkeyTip.textContent = "点击修改";
    window.removeEventListener("keydown", handleRecordKey, { capture: true });
  }

  async function handleRecordKey(e) {
    e.preventDefault();
    e.stopPropagation();

    if (e.key === "Escape") {
      cancelRecording();
      return;
    }

    if (FORBIDDEN_HOTKEYS[e.key]) {
      alert(`【提示】${FORBIDDEN_HOTKEYS[e.key]}，请换其他按键。`);
      cancelRecording();
      return;
    }

    const result = getWinVirtualKey(e);
    if (!result) {
      alert("【提示】请按下有效的功能键 (如 F1~F6, F10~F12, Home, PgUp, 数字小键盘等)");
      cancelRecording();
      return;
    }

    currentHotkeyName = result.name;
    currentHotkeyVK = result.vk;

    localStorage.setItem("autotappo_hotkey_name", currentHotkeyName);
    localStorage.setItem("autotappo_hotkey_vk", currentHotkeyVK.toString());

    if (lblHotkey) lblHotkey.textContent = currentHotkeyName;
    if (btnHotkey) btnHotkey.classList.remove("recording");
    if (hotkeyTip) {
      hotkeyTip.textContent = "修改成功";
      setTimeout(() => {
        hotkeyTip.textContent = "点击修改";
      }, 1500);
    }

    if (isTauri) {
      try {
        await invoke("set_hotkey", { vk: currentHotkeyVK });
      } catch (err) {
        console.error("Failed to set hotkey in backend:", err);
      }
    }

    updateUI({
      running: currentRunning,
      speed_mode: currentSpeed,
      total_sent: localSentCount,
    });

    isRecordingHotkey = false;
    window.removeEventListener("keydown", handleRecordKey, { capture: true });
  }

  if (isTauri) {
    invoke("set_hotkey", { vk: currentHotkeyVK }).catch(() => {});
  }

  // ---------------- 检查更新逻辑 (Gitee 国内直连优先 + 红点感知) ----------------
  function openUpdateModal() {
    if (updateModal) updateModal.classList.add("show");
    if (cachedLatestData) {
      applyUpdateData(cachedLatestData);
    } else {
      checkUpdate({ silent: false });
    }
  }

  function closeUpdateModal() {
    if (updateModal) updateModal.classList.remove("show");
  }

  if (btnBell) btnBell.addEventListener("click", openUpdateModal);
  if (btnModalClose) btnModalClose.addEventListener("click", closeUpdateModal);
  if (updateModal) {
    updateModal.addEventListener("click", (e) => {
      if (e.target === updateModal) {
        closeUpdateModal();
      }
    });
  }

  function compareVersions(v1, v2) {
    const clean1 = (v1 || "").replace(/^v/, "").split(".").map(Number);
    const clean2 = (v2 || "").replace(/^v/, "").split(".").map(Number);
    for (let i = 0; i < Math.max(clean1.length, clean2.length); i++) {
      const num1 = clean1[i] || 0;
      const num2 = clean2[i] || 0;
      if (num1 > num2) return 1;
      if (num1 < num2) return -1;
    }
    return 0;
  }

  async function checkUpdate({ silent = false } = {}) {
    lastCheckTime = Date.now();

    if (!silent) {
      if (updateSpinner) updateSpinner.style.display = "inline-block";
      if (updateStatusBox) updateStatusBox.className = "update-status-box";
      if (updateStatusText) updateStatusText.textContent = "正在通过 Gitee 国内高速通道检查最新版本...";
      if (btnCheckNow) btnCheckNow.disabled = true;
      if (btnCheckText) btnCheckText.textContent = "正在检查...";
      if (changelogArea) changelogArea.style.display = "none";
      if (updateGuideCard) updateGuideCard.style.display = "none";
      if (btnDownload) btnDownload.style.display = "none";
      if (btnGitee) btnGitee.style.display = "none";
      if (btnCheckNow) btnCheckNow.style.display = "inline-flex";
    }

    let latestData = null;
    let source = "Gitee";

    // 带超时的 JSON 拉取：任一源卡住都不会拖慢整体检查
    async function fetchReleaseJson(url, label) {
      const ctrl = new AbortController();
      const timer = setTimeout(() => ctrl.abort(), UPDATE_FETCH_TIMEOUT_MS);
      try {
        const res = await fetch(url, {
          headers: { Accept: "application/json" },
          cache: "no-store",
          signal: ctrl.signal,
        });
        if (!res.ok) {
          console.warn(`[update] ${label} 返回 HTTP ${res.status}`);
          return null;
        }
        const data = await res.json();
        if (data && data.tag_name) return { data, source: label };
        console.warn(`[update] ${label} 无 tag_name 字段`);
      } catch (e) {
        console.warn(`[update] ${label} 获取失败:`, e && e.message ? e.message : e);
      } finally {
        clearTimeout(timer);
      }
      return null;
    }

    // 多源并发查询，然后取版本号最高的一个。
    // 不能「谁先成功就用谁」——某个源滞后（如 Gitee 未发新版）会导致漏报更新。
    const candidates = (
      await Promise.all([
        fetchReleaseJson(GITEE_API_URL, "Gitee"),
        fetchReleaseJson(GITHUB_API_URL, "GitHub"),
        fetchReleaseJson(VERSION_FILE_URLS[0], "Gitee 镜像"),
        fetchReleaseJson(VERSION_FILE_URLS[1], "GitHub 镜像"),
        fetchReleaseJson(VERSION_FILE_URLS[2], "jsDelivr"),
      ])
    ).filter(Boolean);

    if (candidates.length > 0) {
      candidates.sort((a, b) => compareVersions(b.data.tag_name, a.data.tag_name));
      latestData = candidates[0].data;
      source = candidates[0].source;
      console.log(
        `[update] 候选版本: ${candidates.map((c) => `${c.data.tag_name}(${c.source})`).join(", ")} -> 采用 ${latestData.tag_name}`
      );
    }

    cachedLatestData = { data: latestData, source };
    cachedCandidates = candidates;

    if (!silent) {
      if (updateSpinner) updateSpinner.style.display = "none";
      if (btnCheckNow) btnCheckNow.disabled = false;
      if (btnCheckText) btnCheckText.textContent = "再次检查";
    }

    applyUpdateData(cachedLatestData, silent);
  }

  function applyUpdateData(result, silent = false) {
    if (!result) return;
    const { data: latestData, source } = result;

    if (latestData && latestData.tag_name) {
      const latestTag = latestData.tag_name;
      if (lblLatestVersion) lblLatestVersion.textContent = latestTag;

      const cmp = compareVersions(latestTag, CURRENT_VERSION);
      if (cmp > 0) {
        if (updateDot) updateDot.classList.add("active");
        if (updateStatusBox) updateStatusBox.className = "update-status-box has-new";
        if (updateStatusText) {
          updateStatusText.innerHTML = `🎉 <strong>发现新版本 ${latestTag} (${source} 源)</strong>`;
        }

        const body = latestData.body || latestData.description || "包含稳定性优化与体验升级。";
        if (changelogContent) changelogContent.textContent = body;
        if (changelogArea) changelogArea.style.display = "block";

        // 两个仓库分别按 tag 拼出直达链接，避免某个源返回的 html_url 指向另一个平台
        const ghUrl = `${GITHUB_RELEASES_URL}/tag/${latestTag}`;
        const giteeUrl = `${GITEE_RELEASES_URL}/tag/${latestTag}`;

        // 指引卡片展示稳定的 releases 页地址（不带 tag，避免换行折行），
        // 底部按钮才用带 tag 的直达链接
        if (updateGuideCard) {
          updateGuideCard.style.display = "flex";
          if (linkGithubRepo) linkGithubRepo.href = GITHUB_RELEASES_URL;
          if (lblGithubRepoUrl) lblGithubRepoUrl.textContent = GITHUB_RELEASES_URL;
          if (linkGiteeRepo) linkGiteeRepo.href = GITEE_RELEASES_URL;
          if (lblGiteeRepoUrl) lblGiteeRepoUrl.textContent = GITEE_RELEASES_URL;
        }

        // 已确认有新版本，再点「立即检查更新」毫无意义：
        // 收起检查按钮，换成 GitHub / Gitee 两个下载入口
        if (btnCheckNow) btnCheckNow.style.display = "none";
        if (btnDownload) {
          btnDownload.href = ghUrl;
          btnDownload.style.display = "inline-flex";
        }
        if (btnGitee) {
          btnGitee.href = giteeUrl;
          btnGitee.style.display = "inline-flex";
        }
      } else {
        if (updateDot) updateDot.classList.remove("active");
        if (updateStatusBox) updateStatusBox.className = "update-status-box is-latest";
        if (updateStatusText) updateStatusText.textContent = `✨ 当前已是最新版本！(${source} 源同步)`;
        if (changelogArea) changelogArea.style.display = "none";
        if (updateGuideCard) updateGuideCard.style.display = "none";
        if (btnDownload) btnDownload.style.display = "none";
        if (btnGitee) btnGitee.style.display = "none";
        if (btnCheckNow) btnCheckNow.style.display = "inline-flex";
      }
    } else {
      if (updateDot) updateDot.classList.remove("active");
      if (lblLatestVersion) lblLatestVersion.textContent = CURRENT_VERSION;
      if (updateStatusBox) updateStatusBox.className = "update-status-box is-latest";
      if (updateStatusText) {
        updateStatusText.textContent = `✨ 当前运行版本 ${CURRENT_VERSION} 已是最新稳定版！`;
      }
      if (changelogArea) changelogArea.style.display = "none";
      if (updateGuideCard) updateGuideCard.style.display = "none";
      if (btnDownload) btnDownload.style.display = "none";
      if (btnGitee) btnGitee.style.display = "none";
      if (btnCheckNow) btnCheckNow.style.display = "inline-flex";
    }
  }

  if (btnCheckNow) btnCheckNow.addEventListener("click", () => checkUpdate({ silent: false }));

  // ---------------- 一键自更新（下载 → 备份旧 exe → 替换 → 自动重启） ----------------

  // 便携版资产在两个源上都叫 AutoTappo.exe。优先取 Release JSON 里的真实资产地址；
  // 拿不到就按平台约定拼接（GitHub / Gitee 都支持 /releases/download/<tag>/<file>）。
  function buildPortableUrl(channel, tag) {
    const wantGitee = channel === "Gitee";
    const hit = cachedCandidates.find(
      (c) =>
        (c.source === "Gitee" || c.source === "GitHub") === wantGitee &&
        c.data &&
        c.data.tag_name === tag
    );
    const assets = (hit && hit.data && hit.data.assets) || [];
    const pick = assets.find((a) => {
      const n = String((a && a.name) || "").toLowerCase();
      return n.endsWith(".exe") && n.indexOf("setup") < 0 && n.indexOf("install") < 0;
    });
    if (pick && pick.browser_download_url) return pick.browser_download_url;
    return wantGitee
      ? `https://gitee.com/To0Ru/AutoTappo/releases/download/${tag}/AutoTappo.exe`
      : `https://github.com/To0Ru/AutoTappo/releases/download/${tag}/AutoTappo.exe`;
  }

  function setUpdateProgress(text) {
    if (updateStatusBox) updateStatusBox.className = "update-status-box has-new";
    if (updateStatusText) updateStatusText.textContent = text;
    if (updateSpinner) updateSpinner.style.display = "inline-block";
  }

  function setUpdateError(text) {
    if (updateStatusBox) updateStatusBox.className = "update-status-box is-error";
    if (updateStatusText) updateStatusText.textContent = text;
    if (updateSpinner) updateSpinner.style.display = "none";
    // 失败/取消时把手动下载通道露出来
    if (updateGuideCard) updateGuideCard.style.display = "flex";
  }

  // ---------------- 下载进度条（后端 update-progress 事件驱动） ----------------

  function fmtBytes(n) {
    if (!n) return "0 B";
    if (n < 1024 * 1024) return (n / 1024).toFixed(0) + " KB";
    return (n / 1024 / 1024).toFixed(2) + " MB";
  }

  let updateDlStart = 0;

  function showUpdateProgress(show) {
    if (!updateProgressBox) return;
    updateProgressBox.style.display = show ? "flex" : "none";
    if (!show) {
      updateDlStart = 0;
      if (updateProgressFill) {
        updateProgressFill.style.width = "0%";
        updateProgressFill.classList.remove("is-indeterminate");
      }
      if (updateProgressText) updateProgressText.textContent = "准备下载…";
      if (btnUpdateCancel) {
        btnUpdateCancel.disabled = false;
        btnUpdateCancel.textContent = "取消";
      }
    }
  }

  // 事件负载：{ stage: "connect"|"download"|"verify"|"replace"|"launch", got, total }
  function onUpdateProgress(p) {
    if (!p) return;
    const stage = String(p.stage || "");
    const got = Number(p.got) || 0;
    const total = Number(p.total) || 0;

    if (stage === "connect") {
      updateDlStart = Date.now();
      if (updateProgressFill) {
        updateProgressFill.classList.add("is-indeterminate");
        updateProgressFill.style.width = "35%";
      }
      if (updateProgressText) updateProgressText.textContent = "正在连接服务器…";
      return;
    }

    if (stage === "download") {
      if (!updateDlStart) updateDlStart = Date.now();
      if (total > 0) {
        const pct = Math.min(100, (got / total) * 100);
        if (updateProgressFill) {
          updateProgressFill.classList.remove("is-indeterminate");
          updateProgressFill.style.width = pct.toFixed(1) + "%";
        }
        if (updateProgressText) {
          const secs = Math.max(0.2, (Date.now() - updateDlStart) / 1000);
          const speed = got / secs;
          const left = speed > 0 ? Math.max(0, (total - got) / speed) : 0;
          updateProgressText.textContent =
            `${pct.toFixed(0)}%  ${fmtBytes(got)} / ${fmtBytes(total)}  ·  ` +
            `${fmtBytes(speed)}/s  ·  剩约 ${Math.ceil(left)}s`;
        }
      } else {
        // 服务器没给 Content-Length：走不确定态动画
        if (updateProgressFill) updateProgressFill.classList.add("is-indeterminate");
        if (updateProgressText) updateProgressText.textContent = `已下载 ${fmtBytes(got)}（总大小未知）`;
      }
      return;
    }

    if (stage === "verify") {
      if (updateProgressFill) {
        updateProgressFill.classList.remove("is-indeterminate");
        updateProgressFill.style.width = "100%";
      }
      if (updateProgressText) updateProgressText.textContent = "下载完成，正在校验文件…";
      return;
    }

    if (stage === "replace") {
      if (updateProgressText) updateProgressText.textContent = "正在替换程序文件…";
      // 到这一步再取消已经没意义了，锁住按钮避免半途中断
      if (btnUpdateCancel) btnUpdateCancel.disabled = true;
      return;
    }

    if (stage === "launch") {
      if (updateProgressText) updateProgressText.textContent = "正在启动新版本…";
      if (btnUpdateCancel) btnUpdateCancel.disabled = true;
    }
  }

  if (isTauri && window.__TAURI__.event && window.__TAURI__.event.listen) {
    window.__TAURI__.event.listen("update-progress", (ev) => onUpdateProgress(ev.payload));
  }

  if (btnUpdateCancel) {
    btnUpdateCancel.addEventListener("click", async () => {
      btnUpdateCancel.disabled = true;
      btnUpdateCancel.textContent = "取消中…";
      try {
        await invoke("cancel_update");
      } catch (e) {
        console.error("[update] cancel_update failed:", e);
      }
    });
  }

  async function startOneClickUpdate(channel) {
    if (updateBusy) return;

    const latest = cachedLatestData && cachedLatestData.data;
    const tag = (latest && latest.tag_name) || "";
    if (!tag) {
      setUpdateError("还没拿到最新版本号，请先点「立即检查更新」");
      return;
    }

    const url = buildPortableUrl(channel, tag);

    // 普通浏览器预览模式没有原生能力，退化为打开下载地址
    if (!isTauri) {
      window.open(url, "_blank");
      return;
    }

    updateBusy = true;
    if (btnDownload) btnDownload.style.pointerEvents = "none";
    if (btnGitee) btnGitee.style.pointerEvents = "none";
    if (btnCheckNow) btnCheckNow.style.display = "none";
    if (changelogArea) changelogArea.style.display = "none";
    if (updateGuideCard) updateGuideCard.style.display = "none";

    setUpdateProgress(`正在从 ${channel} 下载 ${tag}…`);
    showUpdateProgress(true);

    try {
      await invoke("apply_update", { url });
      // 正常情况下进程已经在重启，几乎不会走到这里
      setUpdateProgress("更新完成，正在重启…");
    } catch (e) {
      updateBusy = false;
      if (btnDownload) btnDownload.style.pointerEvents = "";
      if (btnGitee) btnGitee.style.pointerEvents = "";
      if (btnCheckNow) btnCheckNow.style.display = "inline-flex";
      showUpdateProgress(false);

      const msg = String((e && e.message) || e || "未知错误");
      if (msg.indexOf("已取消") >= 0) {
        setUpdateError("已取消更新，程序保持原样。网速慢的话可以点下面的链接手动下载。");
      } else {
        setUpdateError(`更新失败：${msg}`);
      }
      console.error("[update] apply_update failed:", e);
    }
  }

  function bindOneClickUpdate(btn, channel) {
    if (!btn) return;
    btn.addEventListener("click", (e) => {
      if (!isTauri) return; // 浏览器预览：保留原本的跳转行为
      e.preventDefault();
      startOneClickUpdate(channel);
    });
  }

  bindOneClickUpdate(btnDownload, "GitHub");
  bindOneClickUpdate(btnGitee, "Gitee");

  // ---------------- 界面唤醒 / 打开时的静默自动检测逻辑 ----------------
  function triggerAutoCheck() {
    const now = Date.now();
    if (now - lastCheckTime > 60000) {
      checkUpdate({ silent: true });
    }
  }

  window.addEventListener("focus", triggerAutoCheck);
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "visible") {
      triggerAutoCheck();
    }
  });
  triggerAutoCheck();

  // ==================== 自动开箱 ====================
  const chestCard = document.getElementById("chest-card");
  const chestSwitch = document.getElementById("chest-switch");
  const chestBadge = document.getElementById("chest-badge");
  const chestHint = document.getElementById("chest-hint");
  const chestCount = document.getElementById("chest-count");
  const chestWarn = document.getElementById("chest-warn");
  const btnElevate = document.getElementById("btn-elevate");
  const slotLeft = document.getElementById("slot-left");
  const slotRight = document.getElementById("slot-right");

  // 默认：正式版关闭，浏览器预览模式打开（方便直接双击查看效果）
  const storedChest = localStorage.getItem("autotappo.autoChest");
  let autoChest = (storedChest !== null ? storedChest : isTauri ? "0" : "1") === "1";

  function renderChestSwitch() {
    if (!chestSwitch) return;
    chestSwitch.setAttribute("aria-checked", autoChest ? "true" : "false");
    chestCard.classList.toggle("is-on", autoChest);
    chestBadge.textContent = autoChest ? "已启用" : "已关闭";
  }

  function stateTag(text) {
    const t = (text || "").trim();
    if (!t || t === "-") return ["idle", "—"];
    if (t.indexOf("就绪") >= 0) return ["ready", "就绪"];
    if (t.indexOf("冷却") >= 0) {
      // UIA 路径会把精确剩余时间带过来，例如「冷却 13:34」
      const m = t.match(/(\d{1,2}:\d{2})/);
      return ["cooldown", m ? `冷却 ${m[1]}` : "冷却中"];
    }
    if (t.indexOf("被遮挡") >= 0) return ["cooldown", "被遮挡"];
    if (t.indexOf("无法判定") >= 0 || t.indexOf("截取失败") >= 0) return ["idle", "识别中"];
    return ["idle", t];
  }

  function applySlot(el, text) {
    if (!el) return;
    const res = stateTag(text);
    el.dataset.state = res[0];
    const stateEl = el.querySelector(".slot-state");
    if (stateEl) stateEl.textContent = res[1];
  }

  function fmtMmss(total) {
    const t = Math.max(0, total | 0);
    const mm = String(Math.floor(t / 60)).padStart(2, "0");
    const ss = String(t % 60).padStart(2, "0");
    return `${mm}:${ss}`;
  }

  // 后端为了省 CPU 会一直睡到「到点前 8 秒」，所以剩余时间不可能靠它每秒刷新。
  // 它只给一个「到点时间戳」，界面自己按本地时钟走字，才不会冻住。
  function applySlotWithDeadline(el, text, readyAt) {
    if (!el) return;
    const stateEl = el.querySelector(".slot-state");
    const now = Math.floor(Date.now() / 1000);
    if (readyAt && readyAt > now) {
      el.dataset.state = "cooldown";
      if (stateEl) stateEl.textContent = `冷却 ${fmtMmss(readyAt - now)}`;
      return;
    }
    if (readyAt && readyAt <= now) {
      el.dataset.state = "ready";
      if (stateEl) stateEl.textContent = "即将就绪";
      return;
    }
    applySlot(el, text);
  }

  const STATE_HINT = {
    idle: "待机中",
    "no-pet": "未检测到桌宠，请先启动鸣潮桌宠",
    "no-chest": "宝箱未解锁（累计点击满 1000 后出现）",
    cooldown: "冷却中，到点自动开启",
    occluded: "桌宠被其他窗口遮挡，等待中",
    moving: "桌宠正在移动，稍后重试",
    unknown: "画面识别异常，本轮跳过",
    opened: "刚刚开启了一个宝箱",
    retry: "点击未生效，稍后重试",
    "need-admin": "需要管理员权限才能开箱",
    error: "点击投递失败，稍后重试",
    stopped: "已停止",
  };

  // 开关状态由前端（localStorage）唯一决定，绝不用后端快照反向覆盖：
  // 后端快照只在它自己那一轮循环里更新，刚切换时必然是旧值，
  // 一旦拿它回写就会把刚打开的开关又关掉。
  function applyChestStatus(s) {
    if (!s) return;
    applySlotWithDeadline(slotLeft, s.left, s.left_ready_at);
    applySlotWithDeadline(slotRight, s.right, s.right_ready_at);
    if (chestHint) chestHint.textContent = STATE_HINT[s.state] || s.detail || "待机中";
    if (chestCount) chestCount.textContent = `已开启 ${s.opens_ok || 0} 次`;
    if (chestWarn) chestWarn.hidden = !(autoChest && s.elevated === false);
  }

  async function pollChestStatus() {
    if (!autoChest) return;
    if (!isTauri) {
      applyChestStatus({
        enabled: true,
        elevated: false,
        state: "cooldown",
        detail: "冷却中",
        left: "冷却中",
        right: "就绪",
        opens_ok: 3,
        opens_failed: 0,
      });
      return;
    }
    try {
      const s = await invoke("get_chest_status");
      applyChestStatus(s);
    } catch (e) {
      /* 静默失败，不打扰用户 */
    }
  }

  if (chestSwitch) {
    chestSwitch.addEventListener("click", async () => {
      autoChest = !autoChest;
      localStorage.setItem("autotappo.autoChest", autoChest ? "1" : "0");
      renderChestSwitch();
      if (chestWarn) chestWarn.hidden = true;
      if (isTauri) {
        try {
          await invoke("set_auto_chest", { enabled: autoChest });
        } catch (e) {}
      }
      if (autoChest) pollChestStatus();
    });
  }

  if (btnElevate) {
    btnElevate.addEventListener("click", async () => {
      if (!isTauri) {
        alert("浏览器预览模式：正式版会以管理员权限重启程序");
        return;
      }
      try {
        await invoke("relaunch_as_admin");
      } catch (e) {}
    });
  }

  renderChestSwitch();
  if (isTauri) {
    invoke("set_auto_chest", { enabled: autoChest }).catch(() => {});
    setInterval(pollChestStatus, 1000);
  } else {
    pollChestStatus();
  }

  // 启动状态轮询
  pollStatus();
  if (isTauri) {
    setInterval(pollStatus, 500);
  }

  // 挂载全局测试钩子
  window.testNewVersionUI = function () {
    openUpdateModal();
    applyUpdateData({
      data: {
        tag_name: "v0.2.0",
        html_url: "https://github.com/To0Ru/AutoTappo/releases",
        body: "1. 新增【自动开箱】功能，满额自动抽卡\n2. 优化后台静默避让策略，提升长时间挂机稳定性\n3. 支持更多自定义全局快捷键",
      },
      source: "Gitee",
    });
  };
})();
