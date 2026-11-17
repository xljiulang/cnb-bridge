import { mount } from "@vue/test-utils";
import { describe, expect, it } from "vitest";
import AccountMenu from "./AccountMenu.vue";

const accounts = [
  {
    username: "alice",
    displayName: "爱丽丝",
    avatarUrl: "https://cnb.cool/users/alice/avatar/l",
    lastLoginAt: Date.now() - 60_000,
    active: false,
  },
  {
    username: "bob",
    displayName: "bob",
    avatarUrl: "https://cnb.cool/users/bob/avatar/l",
    lastLoginAt: 0,
    active: true,
  },
];

describe("AccountMenu", () => {
  it("opens the account popover and emits login, remove and scan actions", async () => {
    const wrapper = mount(AccountMenu, {
      attachTo: document.body,
      props: {
        accounts,
      },
    });

    expect(wrapper.find(".account-picker-icon").exists()).toBe(true);
    await wrapper.get(".account-picker-trigger").trigger("click");
    expect(wrapper.get(".account-picker-trigger").attributes("aria-expanded")).toBe(
      "true",
    );
    const rows = [
      ...document.body.querySelectorAll<HTMLElement>(".account-menu-row"),
    ];
    expect(rows).toHaveLength(2);
    expect(document.activeElement).toBe(
      rows[0].querySelector(".account-choice"),
    );
    expect(document.body.textContent).not.toContain("选择账号继续登录");
    expect(document.body.textContent).not.toContain("个已保存账号");
    expect(document.body.textContent).not.toContain("今天");
    expect(document.body.textContent).not.toContain("旧版账号");
    expect(rows[0].textContent).toContain("爱丽丝");
    expect(rows[0].textContent).not.toContain("alice");
    expect(
      rows[0].querySelector("img")?.getAttribute("src"),
    ).toBe("https://cnb.cool/users/alice/avatar/l");
    expect(rows[0].querySelector(".account-choice-copy")).not.toBeNull();
    expect(
      rows[0].lastElementChild?.classList.contains("remove-saved-account"),
    ).toBe(true);

    (rows[0].querySelector(".account-choice") as HTMLElement).click();
    await wrapper.vm.$nextTick();
    expect(wrapper.emitted("login")?.[0]).toEqual(["alice"]);
    expect(document.body.querySelector(".account-menu")).toBeNull();
    expect(document.activeElement).toBe(
      wrapper.get(".account-picker-trigger").element,
    );

    await wrapper.setProps({ busyUsername: "alice", disabled: true });
    await wrapper.get(".account-picker-trigger").trigger("click");
    expect(document.body.querySelector(".account-menu")).toBeNull();
    expect(wrapper.get(".account-picker-trigger").text()).toContain(
      "正在登录…",
    );
    expect(wrapper.find(".account-picker-trigger .spin").exists()).toBe(true);

    await wrapper.setProps({ busyUsername: null, disabled: false });
    await wrapper.get(".account-picker-trigger").trigger("click");
    await wrapper.vm.$nextTick();
    (
      document.body.querySelector(
        '.remove-saved-account[aria-label="移除账号 爱丽丝"]',
      ) as HTMLElement
    ).click();
    expect(wrapper.emitted("remove")?.[0]).toEqual(["alice"]);
    expect(
      (
        document.body.querySelector(
          '.remove-saved-account[aria-label="移除账号 bob"]',
        ) as HTMLButtonElement
      ).disabled,
    ).toBe(true);

    (
      document.body.querySelector(".add-account-action") as HTMLElement
    ).click();
    expect(wrapper.emitted("scan")).toHaveLength(1);
    wrapper.unmount();
  });

  it("closes with Escape and returns focus to the trigger", async () => {
    const wrapper = mount(AccountMenu, {
      attachTo: document.body,
      props: {
        accounts,
      },
    });
    const trigger = wrapper.get(".account-picker-trigger");

    await trigger.trigger("click");
    await trigger.trigger("keydown", { key: "Escape" });

    expect(wrapper.find(".account-menu").exists()).toBe(false);
    expect(document.activeElement).toBe(trigger.element);
    wrapper.unmount();
  });
});
