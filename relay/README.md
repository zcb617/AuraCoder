# AuraCoder Tunnel Relay

第一版 AuraCoder 手机远程访问的通用 WebSocket 隧道服务。

## 本地运行

```powershell
npm install
npm test
npm start
```

默认监听 `0.0.0.0:18080`，健康检查地址为 `http://127.0.0.1:18080/healthz`。

## Docker

```powershell
docker compose build
docker compose up -d
docker compose ps
```

Compose 只把容器端口发布到宿主机 `127.0.0.1:18080`。公网连接必须经过 Nginx：

```text
wss://auracoder.jxrjkf.cn/ws/tunnel
```

## 部署

将整个 `relay` 目录复制到服务器后执行：

```bash
docker compose up -d --build
docker compose ps
curl http://127.0.0.1:18080/healthz
```

Nginx 的 `/ws/tunnel` 应反向代理到 `http://127.0.0.1:18080`。附件 HTTP 路由必须单独配置，支持 POST、GET、DELETE；此 location 不要套用 WebSocket Upgrade：

```nginx
location ^~ /api/mobile/attachments {
    proxy_pass http://127.0.0.1:18080;
    proxy_set_header Host $host;
    proxy_set_header X-Real-IP $remote_addr;
    proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
    proxy_set_header X-Forwarded-Proto $scheme;
    client_max_body_size 10m;
    proxy_request_buffering off;
    proxy_buffering off;
    proxy_read_timeout 300s;
    proxy_send_timeout 300s;
}
```

Relay 会把附件写入 `/data/attachments` 数据卷，默认保留 24 小时；镜像构建时会预创建该目录并设置为运行用户可写，容器重启后仍可由 PC 端按 attachment_key 拉取。上传请求必须带 `x-auracoder-relay-credential`，并在 multipart 中提供 `file`、`tunnel_id`、`device_id`、`batch_id`、`file_name`、`mime_type`、`attachment_kind`。

服务器部署完成后，下面两个检查都应成功：

```bash
curl http://127.0.0.1:18080/healthz
curl https://auracoder.jxrjkf.cn/healthz
```

如果 Nginx 只对外开放 `/ws/tunnel` 而没有转发 `/healthz`，第二条可以跳过；手机和桌面实际使用的仍然是 `wss://auracoder.jxrjkf.cn/ws/tunnel`。
