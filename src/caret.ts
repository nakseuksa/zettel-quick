export interface CaretCoordinates {
  left: number;
  top: number;
  lineHeight: number;
}

const MIRROR_PROPERTIES = [
  "direction",
  "boxSizing",
  "width",
  "overflowX",
  "overflowY",
  "borderTopWidth",
  "borderRightWidth",
  "borderBottomWidth",
  "borderLeftWidth",
  "borderStyle",
  "paddingTop",
  "paddingRight",
  "paddingBottom",
  "paddingLeft",
  "fontStyle",
  "fontVariant",
  "fontWeight",
  "fontStretch",
  "fontSize",
  "fontSizeAdjust",
  "lineHeight",
  "fontFamily",
  "textAlign",
  "textTransform",
  "textIndent",
  "letterSpacing",
  "wordSpacing",
  "tabSize",
] as const;

let mirrorDiv: HTMLDivElement | null = null;

/**
 * 计算 <textarea> 指定字符索引处相对于 textarea 左上角的精确像素坐标
 */
export function getTextareaCaretCoordinates(
  textarea: HTMLTextAreaElement,
  position: number
): CaretCoordinates {
  if (!mirrorDiv) {
    mirrorDiv = document.createElement("div");
    mirrorDiv.id = "textarea-caret-mirror";
    document.body.appendChild(mirrorDiv);
  }

  const style = mirrorDiv.style;
  const computed = window.getComputedStyle(textarea);

  style.whiteSpace = "pre-wrap";
  style.wordWrap = "break-word";
  style.position = "absolute";
  style.visibility = "hidden";
  style.top = "-9999px";
  style.left = "-9999px";

  for (const prop of MIRROR_PROPERTIES) {
    style.setProperty(
      prop.replace(/[A-Z]/g, (m) => `-${m.toLowerCase()}`),
      computed.getPropertyValue(prop.replace(/[A-Z]/g, (m) => `-${m.toLowerCase()}`))
    );
  }
  // 使用 clientWidth 保证换行宽度与出现滚动条后的真实可视文本区一致
  style.width = `${textarea.clientWidth}px`;
  style.overflow = "hidden";

  mirrorDiv.textContent = textarea.value.slice(0, position);

  const marker = document.createElement("span");
  marker.textContent = textarea.value.slice(position, position + 1) || "\u200b";
  mirrorDiv.appendChild(marker);

  const parsedLineHeight = parseFloat(computed.lineHeight);
  const fallbackLineHeight = parseFloat(computed.fontSize) * 1.6 || 24;
  const lineHeight = Number.isFinite(parsedLineHeight) ? parsedLineHeight : fallbackLineHeight;

  const left = marker.offsetLeft - textarea.scrollLeft;
  const top = marker.offsetTop - textarea.scrollTop;

  return { left, top, lineHeight };
}
