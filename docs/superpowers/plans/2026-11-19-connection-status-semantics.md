# Connection Status Semantics Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove speculative UI states and make the connection page distinguish the always-running local HTTP endpoint from the demand-driven CNB workspace and SSH channel.

**Architecture:** Keep the backend snapshot and commands unchanged. Derive only truthful frontend state from `bridge.state`, `bridge.repoSlug`, `bridge.baseUrl`, and account state. The bridge pipeline becomes three actionable steps: account, repository, and cloud workspace/SSH. The local endpoint card reports whether the local gateway actually supplied a `baseUrl`; cached `workspaceSn` remains internal for warm reuse and is not presented as a live connection state.

**Tech Stack:** Vue 3, TypeScript, Vitest, Vite, Tauri 2

**Spec:** Requirements recorded in this plan and confirmed in the 2026-11-19 conversation.

## Global Constraints

- Do not change Tauri commands, `AppSnapshot`, account storage, settings, or backend behavior.
- Keep `workspaceSn` handling unchanged; it is only a warm-start cache.
- Do not imply that the client is polling or automatically reconnecting after `idle`.
- The local OpenAI-compatible HTTP gateway is considered available only when `snapshot.bridge.baseUrl` is non-null.
- Keep actionable states such as `等待扫码绑定`, `等待授权`, `正在连接`, `正在断开`, and real error states.
- All user-facing copy follows the established “桥接” terminology.

---

### Task 1: Simplify the bridge pipeline to truthful live states

**Files:**
- Modify: `ui/src/composables/useDesktopApp.ts:292-361`
- Test: `ui/src/App.test.ts:390-518`

**Interfaces:**
- Consumes: `bridgeState`, `ready`, `idle`, `accountState`, `snapshot.bridge.repoSlug`, and `snapshot.bridge.workspaceSn`.
- Produces: `pipelineSteps` with exactly three entries in this order: `CNB 账号`, `仓库`, `云主机`.

- [ ] **Step 1: Write failing pipeline tests**

Replace the existing cached-workspace and idle-workspace tests with assertions for the new model:

```ts
it("does not present a cached workspace as connected or waiting", async () => {
  const detachedSnapshot = {
    ...snapshot,
    bridge: {
      ...snapshot.bridge,
      state: "idle",
      workspaceSn: "ws-kept",
    },
  };
  mockWindows("main");
  mockIPC(
    (command) => {
      switch (command) {
        case "get_snapshot":
          return detachedSnapshot;
        case "get_logs":
        case "list_http_traces":
        case "list_namespaces":
          return [];
        case "get_cnb_usage":
          return null;
        default:
          return null;
      }
    },
    { shouldMockEvents: true },
  );
  const wrapper = mount(App);
  await flushPromises();

  const steps = wrapper.findAll(".pipeline .step");
  expect(steps).toHaveLength(3);
  const workspaceStep = steps[2];
  expect(workspaceStep.classes()).not.toContain("done");
  expect(workspaceStep.classes()).not.toContain("current");
  expect(workspaceStep.text()).toContain("按需连接");
  expect(workspaceStep.text()).not.toContain("等待重连");
  expect(wrapper.text()).not.toContain("等待建立通道");
  wrapper.unmount();
});

it("shows repository provisioning as on demand before the first bridge", async () => {
  const unpreparedSnapshot = {
    ...snapshot,
    bridge: {
      ...snapshot.bridge,
      state: "idle",
      repoSlug: null,
      workspaceSn: null,
    },
  };
  mockWindows("main");
  mockIPC(
    (command) => {
      switch (command) {
        case "get_snapshot":
          return unpreparedSnapshot;
        case "get_logs":
        case "list_http_traces":
        case "list_namespaces":
          return [];
        case "get_cnb_usage":
          return null;
        default:
          return null;
      }
    },
    { shouldMockEvents: true },
  );
  const wrapper = mount(App);
  await flushPromises();

  expect(wrapper.findAll(".pipeline .step")[1].text()).toContain("按需准备");
  expect(wrapper.text()).not.toContain("等待自动准备");
  wrapper.unmount();
});

it("shows an explicit failure instead of an on-demand cloud state", async () => {
  const failedSnapshot = {
    ...snapshot,
    bridge: {
      ...snapshot.bridge,
      state: "failed",
      workspaceSn: null,
      error: "建立 SSH 通道失败",
    },
  };
  mockWindows("main");
  mockIPC(
    (command) => {
      switch (command) {
        case "get_snapshot":
          return failedSnapshot;
        case "get_logs":
        case "list_http_traces":
        case "list_namespaces":
          return [];
        case "get_cnb_usage":
          return null;
        default:
          return null;
      }
    },
    { shouldMockEvents: true },
  );
  const wrapper = mount(App);
  await flushPromises();

  const workspaceStep = wrapper.findAll(".pipeline .step")[2];
  expect(workspaceStep.classes()).toContain("error");
  expect(workspaceStep.text()).toContain("连接失败");
  expect(workspaceStep.text()).not.toContain("按需连接");
  wrapper.unmount();
});
```

- [ ] **Step 2: Run the pipeline tests and verify they fail**

Run:

```powershell
cd ui
npm test -- --run src/App.test.ts
```

Expected: FAIL because the pipeline currently has four steps, renders `等待重连`, and renders `等待自动准备`.

- [ ] **Step 3: Implement the three-step pipeline**

Replace `pipelineSteps` in `ui/src/composables/useDesktopApp.ts` with:

```ts
const pipelineSteps = computed<ConnectionStep[]>(() => {
  const effectiveBridgeState =
    startingBridge.value && bridgeState.value === "idle"
      ? "preparingKey"
      : bridgeState.value;
  const failed =
    effectiveBridgeState === "failed" ||
    effectiveBridgeState === "needsSshKey";
  const accountDone = accountState.value === "connected";
  const repoDone =
    Boolean(snapshot.value?.bridge.repoSlug) && !repoCreationNeeded.value;
  const tunnelActive = effectiveBridgeState === "connecting";
  const tone = (done: boolean, current: boolean): StepTone => {
    if (done) return "done";
    if (failed) return "error";
    return current ? "current" : "pending";
  };
  const workspaceDetail =
    ready.value
      ? "SSH 已连接"
      : effectiveBridgeState === "stopping"
        ? "正在断开"
        : effectiveBridgeState === "connecting"
          ? "正在连接"
          : effectiveBridgeState === "startingWorkspace"
            ? "正在启动云主机"
            : effectiveBridgeState === "needsSshKey"
              ? "需要注册 SSH 公钥"
              : effectiveBridgeState === "failed"
                ? "连接失败"
                : "按需连接";

  return [
    {
      label: "CNB 账号",
      detail: snapshot.value?.account.displayName || "等待扫码绑定",
      tone: accountDone
        ? "done"
        : accountState.value === "failed"
          ? "error"
          : "current",
    },
    {
      label: "仓库",
      detail: repoDone
        ? snapshot.value?.bridge.repoSlug || ""
        : repoCreationNeeded.value
          ? "需要创建仓库"
          : "按需准备",
      tone: tone(
        repoDone,
        accountDone &&
          !repoDone &&
          (effectiveBridgeState === "preparingRepo" ||
            effectiveBridgeState === "preparingKey"),
      ),
    },
    {
      label: "云主机",
      detail: workspaceDetail,
      tone: tone(
        ready.value,
        effectiveBridgeState === "startingWorkspace" || tunnelActive,
      ),
    },
  ];
});
```

- [ ] **Step 4: Run the pipeline tests and verify they pass**

Run:

```powershell
cd ui
npm test -- --run src/App.test.ts
```

Expected: PASS. Update every four-step assertion in `App.test.ts`, including the startup-stage test that currently checks step index `[3]`, to the new three-step pipeline.

- [ ] **Step 5: Commit the pipeline semantics**

```powershell
git add ui/src/composables/useDesktopApp.ts ui/src/App.test.ts
git commit -m "优化: 简化桥接流程状态"
```

### Task 2: Report the local endpoint from actual gateway availability

**Files:**
- Modify: `ui/src/composables/useDesktopApp.ts:101-160`
- Modify: `ui/src/composables/useDesktopApp.ts:760-805`
- Modify: `ui/src/components/ConnectView.vue:285-300`
- Test: `ui/src/App.test.ts:590-640`

**Interfaces:**
- Consumes: `snapshot.bridge.baseUrl`.
- Produces: `localEndpointReady: ComputedRef<boolean>` on `DesktopApp`.
- Produces: endpoint badge copy `监听中` or `未监听`.

- [ ] **Step 1: Write failing endpoint tests**

Add tests that distinguish gateway availability from cloud SSH readiness:

```ts
it("shows the local endpoint as listening while the cloud bridge is idle", async () => {
  const idleSnapshot = {
    ...snapshot,
    bridge: {
      ...snapshot.bridge,
      state: "idle",
      baseUrl: "http://127.0.0.1:11233/v1",
    },
  };
  mockSnapshotIpc(idleSnapshot);
  const wrapper = mount(App);
  await flushPromises();

  const endpointCard = wrapper.get(".endpoint-card");
  expect(endpointCard.classes()).toContain("is-live");
  expect(endpointCard.get(".badge").text()).toBe("监听中");
  expect(endpointCard.text()).not.toContain("按需启动");
  wrapper.unmount();
});

it("shows the local endpoint as unavailable when the gateway has no base URL", async () => {
  const unavailableSnapshot = {
    ...snapshot,
    bridge: {
      ...snapshot.bridge,
      state: "idle",
      baseUrl: null,
    },
  };
  mockSnapshotIpc(unavailableSnapshot);
  const wrapper = mount(App);
  await flushPromises();

  const endpointCard = wrapper.get(".endpoint-card");
  expect(endpointCard.classes()).not.toContain("is-live");
  expect(endpointCard.get(".badge").text()).toBe("未监听");
  wrapper.unmount();
});
```

If the existing test helper does not accept a snapshot argument, refactor it to:

```ts
function mockSnapshotIpc(current = snapshot) {
  mockWindows("main");
  mockIPC(
    (command) => {
      switch (command) {
        case "get_snapshot":
          return current;
        case "get_logs":
        case "list_http_traces":
        case "list_namespaces":
          return [];
        case "get_cnb_usage":
          return null;
        default:
          return null;
      }
    },
    { shouldMockEvents: true },
  );
}
```

- [ ] **Step 2: Run the endpoint tests and verify they fail**

Run:

```powershell
cd ui
npm test -- --run src/App.test.ts
```

Expected: FAIL because `baseUrl` currently does not control endpoint availability and idle renders `按需启动`.

- [ ] **Step 3: Add `localEndpointReady`**

In `ui/src/composables/useDesktopApp.ts`, add near `baseUrl`:

```ts
const localEndpointReady = computed(
  () => Boolean(snapshot.value?.bridge.baseUrl),
);
```

Expose it from the composable return object:

```ts
localEndpointReady,
```

- [ ] **Step 4: Use real endpoint availability in the card**

Update the endpoint card in `ui/src/components/ConnectView.vue`:

```vue
<article
  class="card connect-card endpoint-card"
  :class="{ 'is-live': app.localEndpointReady }"
>
  <div class="section-head">
    <div>
      <span class="card-label">接入端点</span>
      <h2>
        <Network :size="16" aria-hidden="true" />
        本机 OpenAI 兼容地址
      </h2>
    </div>
    <span
      class="badge"
      :class="app.localEndpointReady ? 'live' : 'muted'"
    >
      {{ app.localEndpointReady ? "监听中" : "未监听" }}
    </span>
  </div>
</article>
```

- [ ] **Step 5: Run the endpoint tests and verify they pass**

Run:

```powershell
cd ui
npm test -- --run src/App.test.ts
```

Expected: PASS. Idle with a gateway URL shows `监听中`; a missing gateway URL shows `未监听`.

- [ ] **Step 6: Commit the endpoint semantics**

```powershell
git add ui/src/composables/useDesktopApp.ts ui/src/components/ConnectView.vue ui/src/App.test.ts
git commit -m "修复: 准确展示本机端点状态"
```

### Task 3: Normalize idle labels and remove vague waiting language

**Files:**
- Modify: `ui/src/format.ts:7-22`
- Modify: `ui/src/components/ConnectView.vue:38-58`
- Modify: `ui/src/components/LogsView.vue:68-75`
- Modify: `ui/src/format.test.ts:26-34`
- Test: `ui/src/App.test.ts:470-518`

**Interfaces:**
- Consumes: `BridgeState::Idle`.
- Produces: user-facing idle copy `按需连接`; no `空闲待命` or `待命` in the connection-page status badge.

- [ ] **Step 1: Write failing label tests**

Update `ui/src/format.test.ts`:

```ts
expect(bridgeLabel("idle")).toBe("按需连接");
```

Add to the cached-workspace test:

```ts
expect(wrapper.get(".title-row .badge").text()).toBe("按需连接");
```

- [ ] **Step 2: Run the label tests and verify they fail**

Run:

```powershell
cd ui
npm test -- --run src/format.test.ts src/App.test.ts
```

Expected: FAIL because `idle` currently renders `空闲待命` in the sidebar and `待命` in the page header.

- [ ] **Step 3: Normalize idle copy**

In `ui/src/format.ts`:

```ts
idle: "按需连接",
```

In the connection-page status ternary in `ui/src/components/ConnectView.vue`, replace:

```vue
? "待命"
```

with:

```vue
? "按需连接"
```

In `ui/src/components/LogsView.vue`, keep the empty state action-oriented:

```vue
<span>完成桥接后，运行信息会显示在这里。</span>
```

- [ ] **Step 4: Run the label tests and verify they pass**

Run:

```powershell
cd ui
npm test -- --run src/format.test.ts src/App.test.ts
```

Expected: PASS. No connection-page idle state uses `空闲待命`, `待命`, `等待重连`, or `等待自动准备`.

- [ ] **Step 5: Commit the normalized idle language**

```powershell
git add ui/src/format.ts ui/src/format.test.ts ui/src/components/ConnectView.vue ui/src/components/LogsView.vue ui/src/App.test.ts
git commit -m "优化: 统一按需连接状态文案"
```

### Task 4: Full frontend verification and debug rebuild

**Files:**
- Verify only; no source edits expected.

**Interfaces:**
- Consumes: the completed UI behavior from Tasks 1-3.
- Produces: a tested production bundle and debug executable containing the final UI.

- [ ] **Step 1: Run all frontend tests**

```powershell
cd ui
npm test -- --run
```

Expected: all test files pass.

- [ ] **Step 2: Run type checking and production build**

```powershell
cd ui
npm run typecheck
npm run build
```

Expected: both commands exit with code `0`.

- [ ] **Step 3: Search for stale speculative copy**

```powershell
rg -n "等待重连|等待自动准备|等待建立通道|按需启动|空闲待命" ui/src --glob "!dist/**" --glob "!**/*.test.ts"
```

Expected: no output. Test fixtures may still use arbitrary status strings and are intentionally excluded.

- [ ] **Step 4: Rebuild the debug desktop client**

```powershell
pwsh -File .\scripts\build-debug.ps1
```

Expected: `D:\github\cyarp-cnb\target\debug\cnb-bridge.exe` is rebuilt with the latest frontend.

- [ ] **Step 5: Manual visual verification**

At a `1319x594` viewport, verify:

- Idle with a cached `workspaceSn`: cloud step is not green and reads `按需连接`.
- Idle without repository state: repository step reads `按需准备`.
- Idle with a local gateway URL: endpoint card is highlighted and reads `监听中`.
- Missing local gateway URL: endpoint card is muted and reads `未监听`.
- Connecting and stopping states still show `正在连接` and `正在断开`.
- Failed state shows `连接失败`, not `按需连接`.
- No page-header idle state reads `待命` or `空闲待命`.

- [ ] **Step 6: Commit verification-only follow-up if manual work requires fixes**

If manual verification required source changes, commit the exact files changed:

```powershell
git add ui/src
git commit -m "修复: 完善连接状态展示"
```

If no fixes were needed, do not create an empty commit.

