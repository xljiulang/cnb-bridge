import { mount } from "@vue/test-utils";
import { describe, expect, it } from "vitest";
import SelectMenu from "./SelectMenu.vue";

const options = [
  { value: "exhaustive", label: "仅异常" },
  { value: "everything", label: "所有" },
];

describe("SelectMenu", () => {
  it("opens a custom listbox and selects an option with the pointer", async () => {
    const wrapper = mount(SelectMenu, {
      attachTo: document.body,
      props: {
        modelValue: "exhaustive",
        options,
        ariaLabel: "HTTP 日志记录策略",
      },
    });

    await wrapper.get(".select-trigger").trigger("click");
    const panel = document.body.querySelector(".select-panel");
    expect(panel?.textContent).toContain("仅异常");
    expect(panel?.textContent).toContain("所有");
    expect(wrapper.get(".select-trigger").attributes("aria-expanded")).toBe("true");

    const allOption = [...document.body.querySelectorAll(".select-option")].find(
      (option) => option.textContent?.includes("所有"),
    ) as HTMLElement;
    allOption.click();
    await wrapper.vm.$nextTick();

    expect(wrapper.emitted("update:modelValue")?.[0]).toEqual(["everything"]);
    expect(document.body.querySelector(".select-panel")).toBeNull();
    wrapper.unmount();
  });

  it("supports keyboard navigation and Escape", async () => {
    const wrapper = mount(SelectMenu, {
      attachTo: document.body,
      props: {
        modelValue: "exhaustive",
        options,
        ariaLabel: "日志级别",
      },
    });
    const trigger = wrapper.get(".select-trigger");

    await trigger.trigger("keydown", { key: "ArrowDown" });
    expect(wrapper.get(".select-trigger").attributes("aria-expanded")).toBe("true");
    const highlighted = () =>
      document.body.querySelector(".select-option.highlighted")?.textContent;
    expect(highlighted()).toContain("仅异常");

    await trigger.trigger("keydown", { key: "ArrowDown" });
    expect(highlighted()).toContain("所有");

    await trigger.trigger("keydown", { key: "Enter" });
    expect(wrapper.emitted("update:modelValue")?.[0]).toEqual(["everything"]);

    await trigger.trigger("keydown", { key: "ArrowDown" });
    await trigger.trigger("keydown", { key: "Escape" });
    expect(document.body.querySelector(".select-panel")).toBeNull();
    wrapper.unmount();
  });

  it("closes when clicking outside and skips disabled options", async () => {
    const wrapper = mount(SelectMenu, {
      attachTo: document.body,
      props: {
        modelValue: "exhaustive",
        options: [
          options[0],
          { value: "disabled", label: "不可用", disabled: true },
          options[1],
        ],
        ariaLabel: "日志级别",
      },
    });
    const trigger = wrapper.get(".select-trigger");

    await trigger.trigger("click");
    await trigger.trigger("keydown", { key: "ArrowDown" });
    expect(document.body.querySelector(".select-option.highlighted")?.textContent).toContain(
      "所有",
    );

    document.body.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true }));
    await wrapper.vm.$nextTick();
    expect(document.body.querySelector(".select-panel")).toBeNull();

    await wrapper.setProps({ disabled: true });
    await trigger.trigger("click");
    expect(document.body.querySelector(".select-panel")).toBeNull();
    wrapper.unmount();
  });
});
