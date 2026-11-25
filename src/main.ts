import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { AutocompleteController } from "./autocomplete";

interface TargetDir {
  name: string;
  path: string;
}

interface UiConfig {
  theme: string;
  width: number;
  height: number;
  font_family: string;
  font_size: number;
  always_on_top_default: boolean;
}

interface AppConfig {
  vault_dir: string;
  targets: TargetDir[];
  ui: UiConfig;
}

interface FrontendBootState {
  config: AppConfig;
  draftContent: string;
  targetIndex: number;
  isRecallingLast: boolean;
  hasLastArchived: boolean;
  pinned: boolean;
}

interface WindowActivatedPayload {
  hasLastArchived: boolean;
  isRecallingLast: boolean;
  pinned: boolean;
}

interface ArchiveResponse {
  outcome:
    | { status: "empty_skipped" }
    | { status: "saved"; file_path: string; filename: string; updated_existing: boolean }
    | { status: "deleted_recalled"; deleted_path: string };
  hasLastArchived: boolean;
}

interface DiscardResponse {
  hasLastArchived: boolean;
}

interface RecallResponse {
  content: string;
  targetIndex: number;
  filename: string;
}

const editor = document.getElementById("editor") as HTMLTextAreaElement;
const popupEl = document.getElementById("autocomplete-popup") as HTMLElement;
const targetBtn = document.getElementById("target-btn") as HTMLButtonElement;
const targetLabel = document.getElementById("target-label") as HTMLElement;
const pinBtn = document.getElementById("pin-btn") as HTMLButtonElement;
const modeBadge = document.getElementById("mode-badge") as HTMLElement;
const statusHint = document.getElementById("status-hint") as HTMLElement;
const charCountEl = document.getElementById("char-count") as HTMLElement;

let currentConfig: AppConfig | null = null;
let targetIndex = 0;
let isRecallingLast = false;
let hasLastArchived = false;
let isPinned = true;
let isComposing = false;
let isArchiving = false;
let draftSyncTimer: number | null = null;
let flashHintTimer: number | null = null;

const autocomplete = new AutocompleteController(editor, popupEl, () => {
  updateStatusUi();
  scheduleDraftSync();
});

function reportFrontendLog(level: "debug" | "info" | "warn" | "error", message: string): void {
  void invoke("log_frontend_event", {
    level,
    scope: "FRONTEND",
    message,
  }).catch(() => {
    // ignore IPC transport failures during teardown
  });
}

window.addEventListener("error", (ev: ErrorEvent) => {
  const loc = ev.filename ? ` (${ev.filename}:${ev.lineno}:${ev.colno})` : "";
  reportFrontendLog("error", `Uncaught Error: ${ev.message}${loc}`);
});

window.addEventListener("unhandledrejection", (ev: PromiseRejectionEvent) => {
  const reason = ev.reason instanceof Error ? `${ev.reason.message} | ${ev.reason.stack ?? ""}` : String(ev.reason);
  reportFrontendLog("error", `Unhandled Promise Rejection: ${reason}`);
});

function applyUiConfig(ui: UiConfig): void {
  const root = document.documentElement;
  if (ui.theme === "dark" || ui.theme === "light") {
    root.setAttribute("data-theme", ui.theme);
  } else {
    root.removeAttribute("data-theme");
  }
  root.style.setProperty("--editor-font-family", ui.font_family);
  root.style.setProperty("--editor-font-size", `${ui.font_size}px`);
}

function countNonWhitespaceChars(text: string): number {
  let count = 0;
  for (const ch of text) {
    if (!/\s/.test(ch)) count++;
  }
  return count;
}

function flashStatusHint(msg: string, durationMs = 2200): void {
  statusHint.textContent = msg;
  if (flashHintTimer !== null) {
    window.clearTimeout(flashHintTimer);
  }
  flashHintTimer = window.setTimeout(() => {
    flashHintTimer = null;
    updateStatusUi();
  }, durationMs);
}

function updateStatusUi(): void {
  if (currentConfig && currentConfig.targets.length > 0) {
    if (targetIndex >= currentConfig.targets.length) {
      targetIndex = 0;
    }
    const t = currentConfig.targets[targetIndex];
    targetLabel.textContent = t.name || t.path || "根目录";
    targetBtn.title = `投递目录：${t.path || "/"}（点击或按 Ctrl+Tab 切换）`;
  }

  pinBtn.classList.toggle("active", isPinned);
  pinBtn.title = isPinned
    ? "当前固定在最外层 · 点击或按 Ctrl+Shift+P 取消置顶并退到其他窗口后面"
    : "当前已退至后方 · 点击或按 Ctrl+Shift+P 恢复固定在最外层";
  modeBadge.classList.toggle("hidden", !isRecallingLast);

  const count = countNonWhitespaceChars(editor.value);
  charCountEl.textContent = `${count} 字`;

  if (flashHintTimer === null) {
    if (isRecallingLast) {
      statusHint.textContent = count === 0 ? "Esc 删除原卡 · Ctrl+Shift+退格 取消" : "Esc 更新原卡";
    } else if (count === 0 && hasLastArchived) {
      statusHint.textContent = "Alt+↑ 续写上一张 · Esc 收起";
    } else if (count === 0) {
      statusHint.textContent = "Esc 收起";
    } else {
      statusHint.textContent = "Esc / Ctrl+Enter 归档";
    }
  }
}

function scheduleDraftSync(): void {
  if (draftSyncTimer !== null) {
    window.clearTimeout(draftSyncTimer);
  }
  draftSyncTimer = window.setTimeout(() => {
    draftSyncTimer = null;
    void invoke("sync_draft", {
      content: editor.value,
      targetIndex,
    });
  }, 600);
}

function cycleTargetDirectory(): void {
  if (!currentConfig || currentConfig.targets.length <= 1) return;
  targetIndex = (targetIndex + 1) % currentConfig.targets.length;
  updateStatusUi();
  scheduleDraftSync();
}

async function performArchive(hideAfter: boolean): Promise<void> {
  if (isArchiving) return;
  isArchiving = true;
  if (draftSyncTimer !== null) {
    window.clearTimeout(draftSyncTimer);
    draftSyncTimer = null;
  }
  autocomplete.close();

  try {
    const res = await invoke<ArchiveResponse>("archive_card", {
      content: editor.value,
      targetIndex,
      hideAfter,
    });
    editor.value = "";
    isRecallingLast = false;
    hasLastArchived = res.hasLastArchived;
    updateStatusUi();

    if (!hideAfter) {
      if (res.outcome.status === "saved") {
        flashStatusHint(`已归档 ${res.outcome.filename}`);
      }
      editor.focus();
    }
  } catch (err) {
    reportFrontendLog("error", `archive_card IPC failed (hideAfter=${hideAfter}): ${String(err)}`);
    flashStatusHint(`保存失败: ${String(err)}`, 4000);
  } finally {
    isArchiving = false;
  }
}

async function performDiscard(): Promise<void> {
  if (draftSyncTimer !== null) {
    window.clearTimeout(draftSyncTimer);
    draftSyncTimer = null;
  }
  autocomplete.close();

  try {
    const res = await invoke<DiscardResponse>("discard_draft", { hideAfter: true });
    editor.value = "";
    isRecallingLast = false;
    hasLastArchived = res.hasLastArchived;
    updateStatusUi();
  } catch (err) {
    reportFrontendLog("error", `discard_draft IPC failed: ${String(err)}`);
    flashStatusHint(`放弃失败: ${String(err)}`);
  }
}

async function performRecallLast(): Promise<void> {
  if (editor.value.trim().length > 0) return;
  try {
    const res = await invoke<RecallResponse>("recall_last_card");
    editor.value = res.content;
    targetIndex = res.targetIndex;
    isRecallingLast = true;
    editor.focus();
    editor.setSelectionRange(editor.value.length, editor.value.length);
    updateStatusUi();
    flashStatusHint(`正在修订：${res.filename}`);
  } catch (err) {
    reportFrontendLog("debug", `recall_last_card rejected: ${String(err)}`);
    flashStatusHint(String(err));
  }
}

async function toggleWindowPin(): Promise<void> {
  try {
    isPinned = await invoke<boolean>("toggle_pin");
    updateStatusUi();
    if (isPinned) {
      flashStatusHint("已固定在最外层");
      editor.focus();
    }
  } catch (err) {
    reportFrontendLog("error", `toggle_pin failed: ${String(err)}`);
  }
}

// 绑定中文输入法预编辑保护，防止拼音选词按 Enter / Esc / 上下键误触发宿主快捷键
editor.addEventListener("compositionstart", () => {
  isComposing = true;
});

editor.addEventListener("compositionend", () => {
  isComposing = false;
  updateStatusUi();
  scheduleDraftSync();
  void autocomplete.refresh();
});

editor.addEventListener("input", () => {
  updateStatusUi();
  scheduleDraftSync();
  if (!isComposing) {
    void autocomplete.refresh();
  }
});

editor.addEventListener("click", () => {
  if (!isComposing) {
    void autocomplete.refresh();
  }
});

editor.addEventListener("scroll", () => {
  if (autocomplete.isOpen()) {
    void autocomplete.refresh();
  }
});

// 粘贴事件拦截：若剪贴板包含截图则自动保存为 PNG 附件并插入纯文本 ![[...png]]
editor.addEventListener("paste", async (e: ClipboardEvent) => {
  const items = e.clipboardData?.items;
  if (!items) return;

  let hasImage = false;
  let hasPlainText = false;
  for (let i = 0; i < items.length; i++) {
    const item = items[i];
    if (item.type.startsWith("image/")) hasImage = true;
    if (item.type === "text/plain") hasPlainText = true;
  }

  if (hasImage && !hasPlainText) {
    e.preventDefault();
    try {
      const syntax = await invoke<string | null>("paste_clipboard_image");
      if (syntax) {
        const start = editor.selectionStart;
        const end = editor.selectionEnd;
        const val = editor.value;
        editor.value = val.slice(0, start) + syntax + val.slice(end);
        const nextPos = start + syntax.length;
        editor.setSelectionRange(nextPos, nextPos);
        updateStatusUi();
        scheduleDraftSync();
        flashStatusHint("截图已存入附件目录");
      }
    } catch (err) {
      reportFrontendLog("error", `paste_clipboard_image failed: ${String(err)}`);
      flashStatusHint(`粘贴图片失败: ${String(err)}`, 3500);
    }
  }
});

window.addEventListener("keydown", (e: KeyboardEvent) => {
  if (isComposing || e.isComposing || e.keyCode === 229) {
    return;
  }

  // 1. 若双链/标签补全菜单处于打开状态，优先拦截上下选择、确认与局部 Esc
  if (autocomplete.isOpen()) {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      autocomplete.moveSelection(1);
      return;
    }
    if (e.key === "ArrowUp") {
      e.preventDefault();
      autocomplete.moveSelection(-1);
      return;
    }
    if ((e.key === "Enter" || e.key === "Tab") && !e.ctrlKey && !e.altKey) {
      e.preventDefault();
      autocomplete.confirmSelection(e.shiftKey);
      return;
    }
    if (e.key === "Escape") {
      e.preventDefault();
      autocomplete.close();
      return;
    }
  }

  // 2. Ctrl+Shift+Enter：连续制卡（保存当前卡片并清空继续写下一张）
  if (e.ctrlKey && e.shiftKey && e.key === "Enter") {
    e.preventDefault();
    void performArchive(false);
    return;
  }

  // 3. Ctrl+Enter 或 Ctrl+S：保存归档并关闭收起
  if ((e.ctrlKey && !e.shiftKey && e.key === "Enter") || (e.ctrlKey && e.key.toLowerCase() === "s")) {
    e.preventDefault();
    void performArchive(true);
    return;
  }

  // 4. Esc（补全未打开时）：自动保存归档并关闭收起
  if (e.key === "Escape") {
    e.preventDefault();
    void performArchive(true);
    return;
  }

  // 5. Alt+ArrowUp：草稿为空时唤回最近保存的一张卡片原地修订
  if (e.altKey && !e.ctrlKey && !e.shiftKey && e.key === "ArrowUp") {
    if (editor.value.trim().length === 0) {
      e.preventDefault();
      void performRecallLast();
      return;
    }
  }

  // 6. Ctrl+Tab：循环切换落盘子目录
  if (e.ctrlKey && e.key === "Tab") {
    e.preventDefault();
    cycleTargetDirectory();
    return;
  }

  // 7. Ctrl+Shift+P：切换窗口置顶
  if (e.ctrlKey && e.shiftKey && e.key.toLowerCase() === "p") {
    e.preventDefault();
    void toggleWindowPin();
    return;
  }

  // 8. Ctrl+Shift+Backspace：主动放弃当前草稿并关闭（不生成文件）
  if (e.ctrlKey && e.shiftKey && e.key === "Backspace") {
    e.preventDefault();
    void performDiscard();
    return;
  }
});

targetBtn.addEventListener("click", () => {
  cycleTargetDirectory();
  editor.focus();
});

pinBtn.addEventListener("click", () => {
  void toggleWindowPin();
});

async function bootstrap(): Promise<void> {
  const t0 = performance.now();
  const boot = await invoke<FrontendBootState>("get_initial_state");
  currentConfig = boot.config;
  targetIndex = boot.targetIndex;
  isRecallingLast = boot.isRecallingLast;
  hasLastArchived = boot.hasLastArchived;
  isPinned = boot.pinned;

  applyUiConfig(boot.config.ui);
  if (boot.draftContent) {
    editor.value = boot.draftContent;
    editor.setSelectionRange(editor.value.length, editor.value.length);
  }
  updateStatusUi();
  editor.focus();
  reportFrontendLog("info", `Frontend UI bootstrapped in ${(performance.now() - t0).toFixed(1)}ms`);

  await listen<WindowActivatedPayload>("app:window-activated", (ev) => {
    hasLastArchived = ev.payload.hasLastArchived;
    isRecallingLast = ev.payload.isRecallingLast;
    isPinned = ev.payload.pinned;
    updateStatusUi();
    requestAnimationFrame(() => {
      editor.focus();
    });
  });

  await listen<AppConfig>("app:config-updated", (ev) => {
    currentConfig = ev.payload;
    applyUiConfig(ev.payload.ui);
    updateStatusUi();
    flashStatusHint("配置已热重载");
  });

  await listen("app:request-archive-and-hide", () => {
    void performArchive(true);
  });
}

void bootstrap();
