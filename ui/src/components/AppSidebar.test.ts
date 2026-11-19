import { mount } from "@vue/test-utils";
import { describe, expect, it, vi } from "vitest";
import type { DesktopApp } from "../composables/useDesktopApp";
import AppSidebar from "./AppSidebar.vue";

const savedAccounts = [
  {
    username: "alice",
    displayName: "爱丽丝",
    avatarUrl: "https://cnb.cool/users/alice/avatar/l",
    lastLoginAt: 200,
    active: false,
  },
  {
    username: "bob",
    displayName: "鲍勃",
    avatarUrl: null,
    lastLoginAt: 100,
    active: false,
  },
];

function app(overrides: Partial<DesktopApp> = {}): DesktopApp {
  return {
    tab: "connect",
    ready: false,
    accountConnected: true,
    accountState: "connected",
    accounts: savedAccounts,
    activeAccount: savedAccounts[0],
    accountLoginUsername: null,
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
      version: "1.0.0",
      logsPath: "",
      httpLogsPath: "",
      models: [],
    },
    openConnect: () => {},
    openSettings: () => {},
    openLogs: () => {},
    openAbout: () => {},
    openProjectHome: () => {},
    login: () => {},
    loginSavedAccount: () => {},
    removeSavedAccount: () => {},
    logout: () => {},
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
    { accountState: "signedOut" as const, label: "未登录" },
    { accountState: "signingIn" as const, label: "等待授权" },
    { accountState: "failed" as const, label: "登录失败" },
  ])(
    "shows the account entry while account state is $accountState",
    ({ accountState, label }) => {
      const wrapper = mount(AppSidebar, {
        props: {
          app: app({
            accountConnected: false,
            accountState,
            activeAccount: null,
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

      expect(wrapper.get(".sidebar-status").text()).toContain(label);
      expect(wrapper.text()).toContain(
        accountState === "signingIn" ? "正在登录" : "登录 CNB",
      );
      wrapper.unmount();
    },
  );

  it("opens saved accounts from the signed-out sidebar entry", async () => {
    const loginSavedAccount = vi.fn();
    const removeSavedAccount = vi.fn();
    const login = vi.fn();
    const wrapper = mount(AppSidebar, {
      attachTo: document.body,
      props: {
        app: app({
          accountConnected: false,
          accountState: "signedOut",
          activeAccount: null,
          statusLabel: "未登录",
          loginSavedAccount,
          removeSavedAccount,
          login,
          snapshot: {
            ...app().snapshot!,
            accounts: savedAccounts,
            account: {
              ...app().snapshot!.account,
              state: "signedOut",
              username: null,
              displayName: null,
              avatarUrl: null,
            },
          },
        }),
      },
    });

    await wrapper.get(".sidebar-status").trigger("click");
    const rows = [
      ...document.body.querySelectorAll<HTMLElement>(".account-menu-row"),
    ];
    expect(rows).toHaveLength(2);
    expect(rows[0].textContent).toContain("爱丽丝");
    (rows[0].querySelector(".account-choice") as HTMLElement).click();
    await wrapper.vm.$nextTick();
    expect(loginSavedAccount).toHaveBeenCalledWith("alice");
    expect(document.body.querySelector(".sidebar-account-menu")).toBeNull();

    await wrapper.get(".sidebar-status").trigger("click");
    (
      document.body.querySelector(
        '.remove-saved-account[aria-label="移除账号 鲍勃"]',
      ) as HTMLElement
    ).click();
    expect(removeSavedAccount).toHaveBeenCalledWith("bob");

    (
      document.body.querySelector(".add-account-action") as HTMLElement
    ).click();
    expect(login).toHaveBeenCalledOnce();
    expect(document.body.textContent).not.toContain("取消扫码");
    wrapper.unmount();
  });

  it("disables the entry while login is in progress", () => {
    const wrapper = mount(AppSidebar, {
      props: {
        app: app({
          accountConnected: false,
          accountState: "signingIn",
          accountLoginUsername: "alice",
          activeAccount: null,
          statusLabel: "等待授权",
          snapshot: {
            ...app().snapshot!,
            account: {
              ...app().snapshot!.account,
              state: "signingIn",
              username: "alice",
              displayName: "爱丽丝",
              avatarUrl: "https://cnb.cool/users/alice/avatar/l",
            },
          },
        }),
      },
    });

    expect(wrapper.get(".sidebar-status").text()).toContain("爱丽丝");
    expect(wrapper.get(".sidebar-status").attributes()).toHaveProperty("disabled");
    wrapper.unmount();
  });

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
      ".logout-account-action",
    ) as HTMLElement;
    expect(logoutButton.textContent).toContain("退出登录");
    logoutButton.click();
    await wrapper.vm.$nextTick();

    expect(logout).toHaveBeenCalledOnce();
    expect(document.body.querySelector(".sidebar-account-menu")).toBeNull();
    wrapper.unmount();
  });

  it("keeps logout available when an active account is not connected", async () => {
    const logout = vi.fn();
    const wrapper = mount(AppSidebar, {
      attachTo: document.body,
      props: {
        app: app({
          accountConnected: false,
          accountState: "failed",
          accounts: [],
          statusLabel: "登录失败",
          logout,
        }),
      },
    });

    await wrapper.get(".sidebar-status").trigger("click");
    const panel = document.body.querySelector(
      ".sidebar-account-menu",
    ) as HTMLElement;
    expect(panel.textContent).toContain("退出登录");
    expect(panel.querySelector(".account-menu-row")).toBeNull();
    wrapper.unmount();
  });

  it("uses the bridge status label in the badge", () => {
    const cases = ["连接就绪", "建立 SSH 通道", "组织未就绪"];

    for (const label of cases) {
      const wrapper = mount(AppSidebar, {
        props: {
          app: app({
            statusLabel: label,
          }),
        },
      });

      expect(wrapper.get(".sidebar-status-badge").text()).toBe(label);
      wrapper.unmount();
    }
  });
});
