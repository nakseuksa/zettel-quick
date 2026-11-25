import { invoke } from "@tauri-apps/api/core";
import { getTextareaCaretCoordinates } from "./caret";

export interface CardSuggestion {
  stem: string;
  display_title: string;
  rel_path: string;
}

export interface TagSuggestion {
  tag: string;
  count: number;
}

type TriggerContext =
  | { kind: "card"; replaceStart: number; replaceEnd: number; query: string }
  | { kind: "tag"; replaceStart: number; replaceEnd: number; query: string };

type CandidateItem =
  | { kind: "card"; data: CardSuggestion }
  | { kind: "tag"; data: TagSuggestion };

const VALID_TAG_CHAR_RE = /^[\p{L}\p{N}_\-/]$/u;

/**
 * 检测光标前是否处于未闭合的 `[[双链` 或 `#标签` 输入上下文中
 */
export function detectTrigger(text: string, cursor: number): TriggerContext | null {
  const lineStart = text.lastIndexOf("\n", Math.max(0, cursor - 1)) + 1;
  const beforeInLine = text.slice(lineStart, cursor);

  // 1. 优先检测双链触发 `[[`（排除图片嵌入 `![[`）
  const openBracketIdx = beforeInLine.lastIndexOf("[[");
  if (openBracketIdx !== -1) {
    const isEmbed = openBracketIdx > 0 && beforeInLine[openBracketIdx - 1] === "!";
    const between = beforeInLine.slice(openBracketIdx + 2);
    if (!isEmbed && !between.includes("]]") && !between.includes("|")) {
      const absStart = lineStart + openBracketIdx;
      return {
        kind: "card",
        replaceStart: absStart,
        replaceEnd: cursor,
        query: between,
      };
    }
  }

  // 2. 检测标签触发 `#`（要求 `#` 位于行首或紧跟空白字符）
  let i = beforeInLine.length - 1;
  while (i >= 0 && VALID_TAG_CHAR_RE.test(beforeInLine[i])) {
    i--;
  }
  if (i >= 0 && beforeInLine[i] === "#") {
    const prevOk = i === 0 || /\s/.test(beforeInLine[i - 1]);
    if (prevOk) {
      const query = beforeInLine.slice(i + 1);
      return {
        kind: "tag",
        replaceStart: lineStart + i,
        replaceEnd: cursor,
        query,
      };
    }
  }

  return null;
}

export class AutocompleteController {
  private textarea: HTMLTextAreaElement;
  private popup: HTMLElement;
  private items: CandidateItem[] = [];
  private selectedIndex = 0;
  private activeTrigger: TriggerContext | null = null;
  private requestSeq = 0;
  private onInserted: () => void;

  constructor(textarea: HTMLTextAreaElement, popup: HTMLElement, onInserted: () => void) {
    this.textarea = textarea;
    this.popup = popup;
    this.onInserted = onInserted;
  }

  public isOpen(): boolean {
    return this.activeTrigger !== null && this.items.length > 0 && !this.popup.classList.contains("hidden");
  }

  public close(): void {
    this.activeTrigger = null;
    this.items = [];
    this.selectedIndex = 0;
    this.popup.classList.add("hidden");
    this.popup.innerHTML = "";
  }

  public async refresh(): Promise<void> {
    const cursor = this.textarea.selectionStart;
    if (cursor !== this.textarea.selectionEnd) {
      this.close();
      return;
    }

    const trigger = detectTrigger(this.textarea.value, cursor);
    if (!trigger) {
      this.close();
      return;
    }

    const seq = ++this.requestSeq;
    try {
      if (trigger.kind === "card") {
        const cards = await invoke<CardSuggestion[]>("search_cards", {
          query: trigger.query,
          limit: 8,
        });
        if (seq !== this.requestSeq) return;
        this.items = cards.map((data) => ({ kind: "card", data }));
      } else {
        const tags = await invoke<TagSuggestion[]>("search_tags", {
          query: trigger.query,
          limit: 8,
        });
        if (seq !== this.requestSeq) return;
        this.items = tags.map((data) => ({ kind: "tag", data }));
      }

      if (this.items.length === 0) {
        this.close();
        return;
      }

      this.activeTrigger = trigger;
      this.selectedIndex = 0;
      this.render();
      this.positionNearCaret(trigger.replaceStart);
    } catch (err) {
      void invoke("log_frontend_event", {
        level: "warn",
        scope: "AUTOCOMPLETE",
        message: `Autocomplete query failed (${trigger.kind}): ${String(err)}`,
      });
      this.close();
    }
  }

  public moveSelection(delta: number): void {
    if (!this.isOpen()) return;
    const len = this.items.length;
    this.selectedIndex = (this.selectedIndex + delta + len) % len;
    this.updateActiveDomHighlight();
  }

  public confirmSelection(withAliasPipe: boolean): boolean {
    if (!this.isOpen() || !this.activeTrigger) return false;
    const chosen = this.items[this.selectedIndex];
    if (!chosen) return false;

    const { replaceStart, replaceEnd } = this.activeTrigger;
    const val = this.textarea.value;

    let insertion = "";
    let extraConsume = 0;

    if (chosen.kind === "card") {
      const { stem, display_title } = chosen.data;
      if (withAliasPipe && display_title && display_title !== stem) {
        insertion = `[[${stem}|${display_title}]]`;
      } else {
        insertion = `[[${stem}]]`;
      }
      // 若光标右侧已有闭合 `]]` 则一并吞并，防止重复产生 `]]]]`
      if (val.slice(replaceEnd, replaceEnd + 2) === "]]") {
        extraConsume = 2;
      }
    } else {
      insertion = `#${chosen.data.tag} `;
    }

    const nextValue = val.slice(0, replaceStart) + insertion + val.slice(replaceEnd + extraConsume);
    const nextCursor = replaceStart + insertion.length;

    this.textarea.value = nextValue;
    this.textarea.setSelectionRange(nextCursor, nextCursor);
    this.close();
    this.onInserted();
    return true;
  }

  private render(): void {
    this.popup.innerHTML = "";

    this.items.forEach((item, idx) => {
      const row = document.createElement("div");
      row.className = `ac-item${idx === this.selectedIndex ? " selected" : ""}`;
      row.setAttribute("role", "option");

      const main = document.createElement("span");
      main.className = "ac-main";
      const sub = document.createElement("span");
      sub.className = "ac-sub";

      if (item.kind === "card") {
        main.textContent = item.data.display_title || item.data.stem;
        sub.textContent = item.data.stem !== item.data.display_title ? item.data.stem : item.data.rel_path;
      } else {
        main.textContent = `#${item.data.tag}`;
        sub.textContent = item.data.count > 0 ? `${item.data.count} 张卡` : "预设";
      }

      row.appendChild(main);
      row.appendChild(sub);

      row.addEventListener("mousedown", (e) => {
        e.preventDefault();
        this.selectedIndex = idx;
        this.confirmSelection(e.shiftKey);
      });

      this.popup.appendChild(row);
    });

    const footer = document.createElement("div");
    footer.className = "ac-footer-tip";
    if (this.activeTrigger?.kind === "card") {
      footer.innerHTML = `<span>Enter 插入双链</span><span>Shift+Enter 带别名</span>`;
    } else {
      footer.innerHTML = `<span>Enter 插入标签</span><span>Esc 取消</span>`;
    }
    this.popup.appendChild(footer);

    this.popup.classList.remove("hidden");
  }

  private updateActiveDomHighlight(): void {
    const rows = this.popup.querySelectorAll<HTMLElement>(".ac-item");
    rows.forEach((row, idx) => {
      if (idx === this.selectedIndex) {
        row.classList.add("selected");
        row.scrollIntoView({ block: "nearest" });
      } else {
        row.classList.remove("selected");
      }
    });
  }

  private positionNearCaret(anchorPos: number): void {
    const container = this.textarea.parentElement;
    if (!container) return;

    const caret = getTextareaCaretCoordinates(this.textarea, anchorPos);
    const containerW = container.clientWidth;
    const containerH = container.clientHeight;
    const popupW = this.popup.offsetWidth || 280;
    const popupH = this.popup.offsetHeight || 160;

    let left = Math.max(8, Math.min(caret.left, containerW - popupW - 8));
    let top = caret.top + caret.lineHeight + 4;

    // 当光标靠近窗口底部空间不足时，智能向上翻转弹出，避免超出 640x360 悬浮卡片边界
    if (top + popupH > containerH - 6 && caret.top - popupH - 4 >= 4) {
      top = caret.top - popupH - 4;
    } else if (top + popupH > containerH - 4) {
      top = Math.max(4, containerH - popupH - 4);
    }

    this.popup.style.left = `${Math.round(left)}px`;
    this.popup.style.top = `${Math.round(top)}px`;
  }
}
