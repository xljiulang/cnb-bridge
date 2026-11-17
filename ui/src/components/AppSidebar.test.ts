import { mount } from "@vue/test-utils";
import { describe, expect, it, vi } from "vitest";
import type { DesktopApp } from "../composables/useDesktopApp";
import AppSidebar from "./AppSidebar.vue";

function app(overrides: Partial<DesktopApp> = {}): DesktopApp {
  return {
    tab: "connect",
    ready: false,
    accountConnected: true,
    sidebarStatusIcon: "pause",
    statusLabel: "空闲待命",
    snapshot: {
      account: {
        state: "connected",
        username: "tester",
        displayName: "测试用户",
        avatarUrl: "https://cnb.cool/users/tester/avatar/l",
        needsOrganization: false,
      },
      bridge: { state: "idle", account: { state: "connected", needsOrganization: false } },
      settings: {
        repoNamespace: "",
        repoName: "cnb-ai-bridge",
        localPort: 11233,
        httpLogFilter: "abnormal",
        launchAtLogin: false,
      },
      version: "0.1.0",
      logsPath: "",
      httpLogsPath: "",
      models: [],
    },
    openConnect: () => {},
    openSettings: () => {},
    openLogs: () => {},
    openAbout: () => {},
    ...overrides,
  } as DesktopApp;
}

describe("AppSidebar", () => {
  it("shows the account avatar, display name and status badge when connected", async () => {
    const openSettings = vi.fn();
    const wrapper = mount(AppSidebar, {
      props: { app: app({ openSettings }) },
    });

    expect(wrapper.get(".status-icon img").attributes("src")).toBe(
      "https://cnb.cool/users/tester/avatar/l",
    );
    expect(wrapper.text()).toContain("测试用户");
    expect(wrapper.get(".sidebar-status-badge").text()).toBe("空闲待命");
    expect(wrapper.text()).not.toContain("11233");
    expect(wrapper.find(".status-icon .lucide-circle-pause").exists()).toBe(false);
    expect(wrapper.find("nav button.active").text()).toContain("连接");
    await wrapper.findAll("nav button")[1].trigger("click");
    expect(openSettings).toHaveBeenCalledOnce();
    wrapper.unmount();
  });

  it.each([
    { accountState: "signedOut" as const, label: "未登录", icon: "scan" as const },
    { accountState: "signingIn" as const, label: "等待授权", icon: "busy" as const },
    { accountState: "failed" as const, label: "登录失败", icon: "error" as const },
  ])(
    "hides the entire status card while account state is $accountState",
    ({ accountState, label, icon }) => {
      const wrapper = mount(AppSidebar, {
        props: {
          app: app({
            accountConnected: false,
            sidebarStatusIcon: icon,
            statusLabel: label,
            snapshot: {
              ...app().snapshot!,
              account: {
                ...app().snapshot!.account,
                state: accountState,
                username: null,
                displayName: null,
                avatarUrl: null,
              },
            },
          }),
        },
      });

      expect(wrapper.find(".sidebar-status").exists()).toBe(false);
      expect(wrapper.text()).not.toContain("等待扫码");
      expect(wrapper.text()).not.toContain(label);
      wrapper.unmount();
    },
  );

  it("falls back to the user icon when the account avatar fails", async () => {
    const wrapper = mount(AppSidebar, {
      props: { app: app() },
    });

    await wrapper.get(".status-icon img").trigger("error");

    expect(wrapper.find(".status-icon img").exists()).toBe(false);
    expect(
      wrapper.find(".status-icon .lucide-circle-user-round").exists(),
    ).toBe(true);
  });

  it("opens account actions and logs out from the sidebar", async () => {
    const logout = vi.fn();
    const wrapper = mount(AppSidebar, {
      attachTo: document.body,
      props: { app: app({ logout }) },
    });

    await wrapper.get(".sidebar-status").trigger("click");
    const logoutButton = document.body.querySelector(
      ".sidebar-logout-action",
    ) as HTMLElement;
    expect(logoutButton.textContent).toContain("退出登录");
    logoutButton.click();
    await wrapper.vm.$nextTick();

    expect(logout).toHaveBeenCalledOnce();
    expect(document.body.querySelector(".sidebar-account-menu")).toBeNull();
    wrapper.unmount();
  });

  it("uses the bridge status label in the badge", () => {
    const cases = [
      { label: "桥接就绪", icon: "live" as const },
      { label: "建立 SSH 通道", icon: "busy" as const },
      { label: "组织未就绪", icon: "busy" as const },
    ];

    for (const entry of cases) {
      const wrapper = mount(AppSidebar, {
        props: {
          app: app({
            statusLabel: entry.label,
            sidebarStatusIcon: entry.icon,
          }),
        },
      });

      expect(wrapper.get(".sidebar-status-badge").text()).toBe(entry.label);
      wrapper.unmount();
    }
  });
});
