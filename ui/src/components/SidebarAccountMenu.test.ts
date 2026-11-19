import { mount } from "@vue/test-utils";
import { describe, expect, it } from "vitest";
import SidebarAccountMenu from "./SidebarAccountMenu.vue";

const props = {
  displayName: "爱丽丝",
  avatarUrl: "https://cnb.cool/users/alice/avatar/l",
  statusLabel: "空闲待命",
  live: false,
  accountState: "connected" as const,
  accounts: [],
  activeAccount: true,
  busyUsername: null,
};

describe("SidebarAccountMenu", () => {
  it("opens a compact action menu without repeating account identity", async () => {
    const wrapper = mount(SidebarAccountMenu, {
      attachTo: document.body,
      props,
    });

    const trigger = wrapper.get(".sidebar-status");
    expect(trigger.attributes("aria-expanded")).toBe("false");
    await trigger.trigger("click");

    expect(trigger.attributes("aria-expanded")).toBe("true");
    const panel = document.body.querySelector(".sidebar-account-menu");
    expect(panel).not.toBeNull();
    expect(panel?.textContent?.trim()).toBe("退出登录");
    expect(panel?.textContent).not.toContain("爱丽丝");
    expect(panel?.textContent).not.toContain("空闲待命");
    expect(panel?.querySelector("img")).toBeNull();
    expect((panel as HTMLElement).style.width).toBe("176px");
    expect(document.activeElement).toBe(
      panel?.querySelector(".logout-account-action"),
    );
    wrapper.unmount();
  });

  it("lists only switchable accounts and keeps logout in the footer", async () => {
    const wrapper = mount(SidebarAccountMenu, {
      attachTo: document.body,
      props: {
        ...props,
        accounts: [
          {
            username: "alice",
            displayName: "爱丽丝",
            avatarUrl: null,
            lastLoginAt: 200,
            active: true,
          },
          {
            username: "bob",
            displayName: "鲍勃",
            avatarUrl: null,
            lastLoginAt: 100,
            active: false,
          },
        ],
      },
    });

    await wrapper.get(".sidebar-status").trigger("click");
    const panel = document.body.querySelector(
      ".sidebar-account-menu",
    ) as HTMLElement;
    const rows = [...panel.querySelectorAll<HTMLElement>(".account-menu-row")];

    expect(panel.textContent).toContain("切换账号");
    expect(panel.textContent).toContain("1");
    expect(rows).toHaveLength(1);
    expect(rows[0].textContent).toContain("鲍勃");
    expect(panel.textContent).not.toContain("爱丽丝");
    expect(rows[0].querySelector(".remove-saved-account")).toBeNull();
    expect(panel.style.width).toBe("320px");
    expect(document.activeElement).toBe(
      rows[0].querySelector(".account-choice"),
    );

    const logout = panel.querySelector(".account-menu-footer .logout-account-action");
    expect(logout?.textContent).toContain("退出登录");
    expect(panel.querySelector(".account-menu-list .logout-account-action")).toBeNull();

    (rows[0].querySelector(".account-choice") as HTMLElement).click();
    await wrapper.vm.$nextTick();
    expect(wrapper.emitted("login")).toEqual([["bob"]]);
    wrapper.unmount();
  });

  it("renders saved accounts when signed out", async () => {
    const account = {
      username: "alice",
      displayName: "爱丽丝",
      avatarUrl: "https://cnb.cool/users/alice/avatar/l",
      lastLoginAt: 200,
      active: false,
    };
    const wrapper = mount(SidebarAccountMenu, {
      attachTo: document.body,
      props: {
        ...props,
        displayName: "登录 CNB",
        avatarUrl: null,
        statusLabel: "未登录",
        accountState: "signedOut",
        accounts: [account],
        activeAccount: false,
      },
    });

    await wrapper.get(".sidebar-status").trigger("click");
    const panel = document.body.querySelector(
      ".sidebar-account-menu",
    ) as HTMLElement;
    expect(panel.textContent).toContain("已保存账号");
    expect(panel.textContent).toContain("爱丽丝");
    expect(panel.textContent).toContain("扫码登录 CNB 账号");
    expect(panel.textContent).not.toContain("退出登录");
    expect(panel.textContent).not.toContain("取消扫码");
    wrapper.unmount();
  });

  it("emits logout once and closes the panel", async () => {
    const wrapper = mount(SidebarAccountMenu, {
      attachTo: document.body,
      props,
    });
    await wrapper.get(".sidebar-status").trigger("click");
    await wrapper.vm.$nextTick();

    (
      document.body.querySelector(".logout-account-action") as HTMLElement
    ).click();
    await wrapper.vm.$nextTick();

    expect(wrapper.emitted("logout")).toHaveLength(1);
    expect(document.body.querySelector(".sidebar-account-menu")).toBeNull();
    wrapper.unmount();
  });

  it("closes with Escape and returns focus to the status card", async () => {
    const wrapper = mount(SidebarAccountMenu, {
      attachTo: document.body,
      props,
    });
    const trigger = wrapper.get(".sidebar-status");
    await trigger.trigger("click");

    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    await wrapper.vm.$nextTick();

    expect(document.body.querySelector(".sidebar-account-menu")).toBeNull();
    expect(document.activeElement).toBe(trigger.element);
    wrapper.unmount();
  });

  it("closes when clicking outside the panel", async () => {
    const wrapper = mount(SidebarAccountMenu, {
      attachTo: document.body,
      props,
    });
    await wrapper.get(".sidebar-status").trigger("click");

    document.body.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true }));
    await wrapper.vm.$nextTick();

    expect(document.body.querySelector(".sidebar-account-menu")).toBeNull();
    wrapper.unmount();
  });
});
