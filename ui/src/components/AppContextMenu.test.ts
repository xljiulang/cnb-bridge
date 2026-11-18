import { flushPromises, mount } from "@vue/test-utils";
import type { VueWrapper } from "@vue/test-utils";
import { readText, writeText } from "@tauri-apps/plugin-clipboard-manager";
import { afterEach, describe, expect, it, vi } from "vitest";
import AppContextMenu from "./AppContextMenu.vue";

vi.mock("@tauri-apps/plugin-clipboard-manager", () => ({
  readText: vi.fn(),
  writeText: vi.fn(),
}));

let wrappers: VueWrapper[] = [];

function mountMenu() {
  const wrapper = mount(AppContextMenu);
  wrappers.push(wrapper);
  return wrapper;
}

function dispatchShortcut(
  key: string,
  options: KeyboardEventInit = {},
): KeyboardEvent {
  const event = new KeyboardEvent("keydown", {
    key,
    bubbles: true,
    cancelable: true,
    ...options,
  });
  window.dispatchEvent(event);
  return event;
}

function openContextMenu(target: Element, x = 100, y = 80): MouseEvent {
  const event = new MouseEvent("contextmenu", {
    bubbles: true,
    cancelable: true,
    clientX: x,
    clientY: y,
  });
  target.dispatchEvent(event);
  return event;
}

afterEach(() => {
  wrappers.forEach((wrapper) => wrapper.unmount());
  wrappers = [];
  document.body.innerHTML = "";
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  vi.clearAllMocks();
});

describe("AppContextMenu", () => {
  it("blocks browser shortcuts while preserving editing shortcuts", async () => {
    mountMenu();
    await flushPromises();

    for (const key of ["F5", "F11", "F12"]) {
      expect(dispatchShortcut(key).defaultPrevented).toBe(true);
    }
    for (const key of ["r", "p", "s", "o", "u"]) {
      expect(
        dispatchShortcut(key, { ctrlKey: true }).defaultPrevented,
      ).toBe(true);
    }
    for (const key of ["R", "I", "J", "C"]) {
      expect(
        dispatchShortcut(key, { ctrlKey: true, shiftKey: true })
          .defaultPrevented,
      ).toBe(true);
    }
    for (const key of ["+", "-", "0"]) {
      expect(
        dispatchShortcut(key, { ctrlKey: true }).defaultPrevented,
      ).toBe(true);
    }
    for (const key of ["ArrowLeft", "ArrowRight"]) {
      expect(
        dispatchShortcut(key, { altKey: true }).defaultPrevented,
      ).toBe(true);
    }
    expect(
      dispatchShortcut("c", { ctrlKey: true }).defaultPrevented,
    ).toBe(false);
    expect(
      dispatchShortcut("v", { ctrlKey: true }).defaultPrevented,
    ).toBe(false);
    expect(dispatchShortcut("a").defaultPrevented).toBe(false);

  });

  it("copies the selected text from an editable field", async () => {
    const input = document.createElement("input");
    input.value = "hello world";
    document.body.append(input);
    input.focus();
    input.setSelectionRange(0, 5);
    mountMenu();
    const event = openContextMenu(input);

    expect(event.defaultPrevented).toBe(true);
    await flushPromises();
    expect(document.body.querySelector('[role="menu"]')).not.toBeNull();
    expect(
      [...document.body.querySelectorAll<HTMLElement>('[role="menuitem"]')].map(
        (item) => item.textContent?.trim(),
      ),
    ).toEqual(["撤销", "重做", "剪切", "复制", "粘贴", "全选"]);

    document.body
      .querySelector<HTMLButtonElement>('[data-action="copy"]')
      ?.click();
    await flushPromises();

    expect(writeText).toHaveBeenCalledWith("hello");
    expect(document.body.querySelector('[role="menu"]')).toBeNull();
    expect(document.activeElement).toBe(input);
  });

  it("renders an accessible 16px icon before every menu label", async () => {
    const input = document.createElement("input");
    input.value = "hello world";
    document.body.append(input);
    input.focus();
    input.setSelectionRange(0, 5);
    mountMenu();
    openContextMenu(input);
    await flushPromises();

    const expectedIcons = {
      undo: "Undo2",
      redo: "Redo2",
      cut: "Scissors",
      copy: "Copy",
      paste: "ClipboardPaste",
      selectAll: "TextSelect",
    };

    for (const [action, iconName] of Object.entries(expectedIcons)) {
      const item = document.body.querySelector<HTMLButtonElement>(
        `[data-action="${action}"]`,
      );
      const icon = item?.querySelector("svg");

      expect(icon, `${action} should render an icon`).not.toBeNull();
      expect(icon?.dataset.icon).toBe(iconName);
      expect(icon?.getAttribute("aria-hidden")).toBe("true");
      expect(icon?.getAttribute("width")).toBe("16");
      expect(icon?.getAttribute("height")).toBe("16");
    }
  });

  it("pastes clipboard text and notifies the editable field", async () => {
    const textarea = document.createElement("textarea");
    textarea.value = "hello world";
    document.body.append(textarea);
    textarea.focus();
    textarea.setSelectionRange(5, 5);
    const inputListener = vi.fn();
    textarea.addEventListener("input", inputListener);
    vi.mocked(readText).mockResolvedValue(",");
    mountMenu();
    openContextMenu(textarea);
    await flushPromises();

    document.body
      .querySelector<HTMLButtonElement>('[data-action="paste"]')
      ?.click();
    await flushPromises();

    expect(textarea.value).toBe("hello, world");
    expect(textarea.selectionStart).toBe(6);
    expect(textarea.selectionEnd).toBe(6);
    expect(inputListener).toHaveBeenCalledTimes(1);
    expect(document.activeElement).toBe(textarea);
  });

  it("closes on Escape and returns focus to the original target", async () => {
    const input = document.createElement("input");
    document.body.append(input);
    input.focus();
    mountMenu();
    openContextMenu(input);
    await flushPromises();

    document.activeElement?.dispatchEvent(
      new KeyboardEvent("keydown", {
        key: "Escape",
        bubbles: true,
        cancelable: true,
      }),
    );
    await flushPromises();

    expect(document.body.querySelector('[role="menu"]')).toBeNull();
    expect(document.activeElement).toBe(input);
  });

  it("navigates enabled actions with arrow keys and activates with Enter", async () => {
    const textarea = document.createElement("textarea");
    textarea.value = "content";
    document.body.append(textarea);
    textarea.focus();
    mountMenu();
    openContextMenu(textarea);
    await flushPromises();

    window.dispatchEvent(
      new KeyboardEvent("keydown", {
        key: "ArrowDown",
        bubbles: true,
        cancelable: true,
      }),
    );
    const activeItem = document.activeElement as HTMLButtonElement;
    expect(activeItem.dataset.action).toBe("selectAll");

    activeItem.dispatchEvent(
      new KeyboardEvent("keydown", {
        key: "Enter",
        bubbles: true,
        cancelable: true,
      }),
    );
    await flushPromises();

    expect(textarea.selectionStart).toBe(0);
    expect(textarea.selectionEnd).toBe("content".length);
    expect(document.body.querySelector('[role="menu"]')).toBeNull();
    expect(document.activeElement).toBe(textarea);
  });

  it("clamps the menu inside the viewport", async () => {
    vi.stubGlobal("innerWidth", 320);
    vi.stubGlobal("innerHeight", 200);
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue({
      x: 0,
      y: 0,
      width: 180,
      height: 160,
      top: 0,
      right: 180,
      bottom: 160,
      left: 0,
      toJSON: () => ({}),
    });
    const input = document.createElement("input");
    document.body.append(input);
    mountMenu();
    openContextMenu(input, 500, 300);
    await flushPromises();

    const menu = document.body.querySelector<HTMLElement>('[role="menu"]');
    expect(menu?.style.left).toBe("132px");
    expect(menu?.style.top).toBe("32px");
  });

  it("closes when the user clicks outside the menu", async () => {
    const input = document.createElement("input");
    document.body.append(input);
    mountMenu();
    openContextMenu(input);
    await flushPromises();

    document.body.dispatchEvent(
      new PointerEvent("pointerdown", { bubbles: true, cancelable: true }),
    );
    await flushPromises();

    expect(document.body.querySelector('[role="menu"]')).toBeNull();
  });
});
