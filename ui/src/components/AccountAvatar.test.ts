import { mount } from "@vue/test-utils";
import { describe, expect, it } from "vitest";
import AccountAvatar from "./AccountAvatar.vue";

describe("AccountAvatar", () => {
  it("renders the CNB avatar URL when one is available", () => {
    const wrapper = mount(AccountAvatar, {
      props: {
        avatarUrl: "https://cnb.cool/users/alice/avatar/l",
      },
    });

    expect(wrapper.get("img").attributes("src")).toBe(
      "https://cnb.cool/users/alice/avatar/l",
    );
    expect(wrapper.get("img").attributes("referrerpolicy")).toBe("no-referrer");
  });

  it("falls back to a user icon when the avatar fails to load", async () => {
    const wrapper = mount(AccountAvatar, {
      props: {
        avatarUrl: "https://cnb.cool/users/alice/avatar/l",
      },
    });

    await wrapper.get("img").trigger("error");

    expect(wrapper.find("img").exists()).toBe(false);
    expect(wrapper.find("svg").exists()).toBe(true);
  });

  it("resets the image error state when the avatar URL changes", async () => {
    const wrapper = mount(AccountAvatar, {
      props: {
        avatarUrl: "https://cnb.cool/users/alice/avatar/l",
      },
    });
    await wrapper.get("img").trigger("error");

    await wrapper.setProps({
      avatarUrl: "https://cnb.cool/users/bob/avatar/l",
    });

    expect(wrapper.get("img").attributes("src")).toBe(
      "https://cnb.cool/users/bob/avatar/l",
    );
  });

  it("shows the fallback icon when no avatar URL exists", () => {
    const wrapper = mount(AccountAvatar, {
      props: {
        avatarUrl: null,
      },
    });

    expect(wrapper.find("img").exists()).toBe(false);
    expect(wrapper.find("svg").exists()).toBe(true);
  });
});
