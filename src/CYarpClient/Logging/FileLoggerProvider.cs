using Microsoft.Extensions.Logging;
using System;
using System.Globalization;
using System.IO;
using System.Text;

namespace CYarpClient
{
    /// <summary>
    /// Minimal daily rolling file logger. Replaces Serilog so the client stays
    /// reflection free and therefore Native AOT friendly.
    /// </summary>
    internal sealed class FileLoggerProvider : ILoggerProvider
    {
        private const LogLevel MinimumLevel = LogLevel.Information;

        private readonly string directory;
        private readonly object syncRoot = new();
        private StreamWriter? writer;
        private DateOnly writerDate;
        private bool disposed;

        public FileLoggerProvider(string directory)
        {
            this.directory = directory;
        }

        public ILogger CreateLogger(string categoryName)
        {
            return new FileLogger(this, categoryName);
        }

        internal void Write(string categoryName, LogLevel logLevel, string message)
        {
            var line = string.Create(CultureInfo.InvariantCulture, $"{DateTimeOffset.Now:yyyy-MM-dd HH:mm:ss.fff zzz} [{Level(logLevel)}] {categoryName}{Environment.NewLine}{message}{Environment.NewLine}");

            lock (this.syncRoot)
            {
                if (this.disposed)
                {
                    return;
                }

                this.EnsureWriter();
                this.writer!.Write(line);
            }
        }

        private void EnsureWriter()
        {
            var today = DateOnly.FromDateTime(DateTime.Now);
            if (this.writer != null && this.writerDate == today)
            {
                return;
            }

            this.writer?.Dispose();

            Directory.CreateDirectory(this.directory);
            var path = Path.Combine(this.directory, string.Create(CultureInfo.InvariantCulture, $"log{today:yyyyMMdd}.txt"));
            var stream = new FileStream(path, FileMode.Append, FileAccess.Write, FileShare.Read);
            this.writer = new StreamWriter(stream, new UTF8Encoding(encoderShouldEmitUTF8Identifier: false)) { AutoFlush = true };
            this.writerDate = today;
        }

        public void Dispose()
        {
            lock (this.syncRoot)
            {
                this.disposed = true;
                this.writer?.Dispose();
                this.writer = null;
            }
        }

        private static string Level(LogLevel logLevel)
        {
            return logLevel switch
            {
                LogLevel.Trace => "TRC",
                LogLevel.Debug => "DBG",
                LogLevel.Information => "INF",
                LogLevel.Warning => "WRN",
                LogLevel.Error => "ERR",
                LogLevel.Critical => "CRT",
                _ => "UNK",
            };
        }

        private sealed class FileLogger(FileLoggerProvider provider, string categoryName) : ILogger
        {
            public IDisposable? BeginScope<TState>(TState state) where TState : notnull
            {
                return null;
            }

            public bool IsEnabled(LogLevel logLevel)
            {
                return logLevel >= MinimumLevel;
            }

            public void Log<TState>(LogLevel logLevel, EventId eventId, TState state, Exception? exception, Func<TState, Exception?, string> formatter)
            {
                if (this.IsEnabled(logLevel) == false)
                {
                    return;
                }

                var message = formatter(state, exception);
                if (exception != null)
                {
                    message = $"{message}{Environment.NewLine}{exception}";
                }

                provider.Write(categoryName, logLevel, message);
            }
        }
    }
}
