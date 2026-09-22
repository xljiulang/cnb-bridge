# Build stage: compile the patched CYarpServer from source.
# Put the CYarp source tree (with cyarp-cnb-token.patch applied) in the build context.
FROM mcr.microsoft.com/dotnet/sdk:8.0 AS build
WORKDIR /src
COPY . .
RUN dotnet publish Host/CYarpServer/CYarpServer.csproj -c Release -o /app

# Runtime stage
FROM mcr.microsoft.com/dotnet/aspnet:8.0 AS runtime
WORKDIR /app
COPY --from=build /app .
# HTTP only; TLS terminates at Caddy in front of this container.
ENV ASPNETCORE_URLS=http://+:8080
EXPOSE 8080
ENTRYPOINT ["dotnet", "CYarpServer.dll"]