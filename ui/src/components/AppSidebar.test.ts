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
  it("shows pause icon for idle and keeps the selected navigation", async () => {
    const openSettings = vi.fn();
    const wrapper = mount(AppSidebar, {
      props: { app: app({ openSettings }) },
    });

    expect(wrapper.find(".status-icon .lucide-circle-pause").exists()).toBe(true);
    expect(wrapper.text()).toContain("空闲待命");
    expect(wrapper.text()).toContain("测试用户");
    expect(wrapper.find("nav button.active").text()).toContain("连接");
    await wrapper.findAll("nav button")[1].trigger("click");
    expect(openSettings).toHaveBeenCalledOnce();
    wrapper.unmount();
  });

  it("shows the scan icon when signed out", () => {
    const wrapper = mount(AppSidebar, {
      props: {
        app: app({
          accountConnected: false,
          sidebarStatusIcon: "scan",
          statusLabel: "未登录",
        }),
      },
    });

    expect(wrapper.find(".status-icon .lucide-scan-qr-code").exists()).toBe(true);
    expect(wrapper.text()).toContain("等待扫码");
    wrapper.unmount();
  });
});
