using CYarp.Server;
using CYarpServer.Quota;
using System;
using System.Collections.Generic;
using System.Threading;
using System.Threading.Tasks;
using Xunit;

namespace CYarp.Cnb.Tests
{
    /// <summary>
    /// Unit tests of the connect-event plumbing: the CYarp state callback turns a connect into a
    /// signal, and the signal is a plain queue in front of the probe.
    /// <para>
    /// The contract these pin down matters for the state pipeline's health: both callbacks must
    /// complete synchronously and must never probe inline, because CYarp calls the storages one at
    /// a time from a single consumer loop.
    /// </para>
    /// </summary>
    public sealed class AiQuotaProbeTriggerTests
    {
        [Fact]
        public async Task WriteClientStateAsync_OnConnect_QueuesTheId()
        {
            var signal = new AiQuotaProbeSignal();
            var trigger = new AiQuotaProbeTrigger(signal);

            var task = trigger.WriteClientStateAsync(State("orgA/repoA", connected: true), CancellationToken.None);

            // Synchronous completion is the point: a state callback that awaited a probe would stall
            // every later connect/disconnect event behind it.
            Assert.True(task.IsCompletedSuccessfully);
            Assert.Equal(["orgA/repoA"], await ReadAsync(signal, expected: 1));
        }

        [Fact]
        public async Task WriteClientStateAsync_OnDisconnect_QueuesNothing()
        {
            var signal = new AiQuotaProbeSignal();
            var trigger = new AiQuotaProbeTrigger(signal);

            // A disconnect keeps the snapshot as is and needs no probe: only connected workspaces are
            // listed by GET /, and a reconnect arrives as a fresh connect event.
            await trigger.WriteClientStateAsync(State("orgA/repoA", connected: false), CancellationToken.None);

            Assert.Empty(await ReadAsync(signal, expected: 0, timeout: TimeSpan.FromMilliseconds(250)));
        }

        [Fact]
        public async Task WriteClientStateAsync_QueuesEveryConnect_LeavingDeduplicationToTheProbe()
        {
            var signal = new AiQuotaProbeSignal();
            var trigger = new AiQuotaProbeTrigger(signal);

            // Two connects for one id are two events, not one: the trigger stays deliberately dumb,
            // and the probe service decides whether a second probe is worth opening a tunnel for.
            await trigger.WriteClientStateAsync(State("orgA/repoA", connected: true), CancellationToken.None);
            await trigger.WriteClientStateAsync(State("orgA/repoA", connected: true), CancellationToken.None);

            Assert.Equal(["orgA/repoA", "orgA/repoA"], await ReadAsync(signal, expected: 2));
        }

        [Fact]
        public async Task InitClientStatesAsync_QueuesNothing()
        {
            var signal = new AiQuotaProbeSignal();
            var trigger = new AiQuotaProbeTrigger(signal);

            // The initialisation exists for persistence ("mark every client offline on startup") and
            // has nothing to say about this feature.
            var task = trigger.InitClientStatesAsync(CancellationToken.None);

            Assert.True(task.IsCompletedSuccessfully);
            Assert.Empty(await ReadAsync(signal, expected: 0, timeout: TimeSpan.FromMilliseconds(250)));
        }

        private static ClientState State(string clientId, bool connected)
        {
            return new ClientState
            {
                Client = new FakeClient(clientId),
                IsConnected = connected,
            };
        }

        /// <summary>
        /// Reads up to <paramref name="expected"/> ids, giving up after a short window so the
        /// "nothing was queued" cases finish instead of waiting for an item that never comes.
        /// </summary>
        private static async Task<IReadOnlyList<string>> ReadAsync(AiQuotaProbeSignal signal, int expected, TimeSpan? timeout = null)
        {
            var ids = new List<string>();
            if (expected == 0)
            {
                // Nothing is expected, so watch for a moment and report whatever shows up (usually
                // nothing). Opening the reader is still what makes a stray write visible.
                using var watch = new CancellationTokenSource(timeout ?? TimeSpan.FromMilliseconds(250));
                try
                {
                    await foreach (var id in signal.ReadAllAsync(watch.Token))
                    {
                        ids.Add(id);
                    }
                }
                catch (OperationCanceledException)
                {
                    // Stayed empty for as long as the test cared to watch.
                }

                return ids;
            }

            using var cts = new CancellationTokenSource(timeout ?? TimeSpan.FromSeconds(5d));

            try
            {
                await foreach (var id in signal.ReadAllAsync(cts.Token))
                {
                    ids.Add(id);
                    if (ids.Count == expected)
                    {
                        break;
                    }
                }
            }
            catch (OperationCanceledException)
            {
                // The queue stayed empty for as long as the test cared to watch.
            }

            return ids;
        }
    }
}
