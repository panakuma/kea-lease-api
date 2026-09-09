# kea-lease-api

## 何ができるの？
ISC Kea DHCPサーバのリース情報を、バックエンドのMySQLもしくはMariaDBから取得して、JSON形式で返してくれるHTTP APIサーバが建てられます。
IPv4 (`lease4`) とIPv6 (`lease6`) の両方に対応しています。Prometheus用のメトリクスも吐けます。

## 使い方
1. リポジトリをクローンしてビルドします
```sh
git clone https://github.com/panakuma/kea-lease-api.git
cd kea-lease-api
cargo build --release
```
`target/release` に実行ファイル( `kea_lease-db_api` )が生成されるので、適当なディレクトリにコピーします。
ビルドにデータベースは不要です。

2. 設定ファイルの準備
`config.toml.example` を `config.toml` としてコピーします。
適当なテキストエディタで開いて、DBへの接続情報を記入します。
任意でリッスンするIPやポート番号を変更します。

```toml
[general]
bind_addr = "[::]"
bind_port = 3000
# request_timeout_secs = 30   # 1リクエストの上限時間。超えると408を返します
# max_limit = 10000           # limitパラメータの上限 = 1回に返す最大行数
# trust_proxy_header = false  # trueにするとX-Forwarded-ForをログのクライアントIPに使います

[database]
host = "192.0.2.1"
port = 3306
user = "username"
password = "password"
database = "kea database name"
# max_connections = 5
# connect_timeout_secs = 10
```

パスワードを設定ファイルに置きたくない場合は、環境変数 `KEA_LEASE_API_DB_PASSWORD` で上書きできます。

3. 実行
```sh
./kea_lease-db_api
```
設定ファイルは以下の順に探します。systemdなどから起動する場合は、カレントディレクトリに依存しない指定ができます。

1. 第1引数 (`./kea_lease-db_api /etc/kea-lease-api/config.toml`)
2. 環境変数 `KEA_LEASE_API_CONFIG`
3. カレントディレクトリの `config.toml`

ログの詳細度は環境変数 `RUST_LOG` で変えられます (既定は `info`)。

## API仕様

| パス | 内容 |
| --- | --- |
| `GET /` `GET /leases` | IPv4リースの一覧 |
| `GET /leases/count` | IPv4リースの件数 |
| `GET /leases/{address}` | 指定したIPv4アドレスのリース1件 |
| `GET /leases6` | IPv6リースの一覧 |
| `GET /leases6/count` | IPv6リースの件数 |
| `GET /leases6/{address}` | 指定したIPv6アドレスのリース1件 |
| `GET /stats` | サブネット別・state別の集計 (JSON) |
| `GET /metrics` | 同じ集計をPrometheus形式で |
| `GET /healthz` | DBへの疎通確認 |

### 既定では「今使われているリース」だけを返します

ここが v0.1 からの一番大きな変更点です。

Keaは失効したリースを`lease4`から削除しません。`state`列の値を`expired-reclaimed`(2)に書き換えて残します。
`declined`(1)、`released`(3)、`registered`(4)も同様に残ります。
そのため単純に全行を数えると、DHCPサーバを動かし続けるほど実際の使用数から離れていきます。

そこで `/leases` と `/leases/count` は、既定で **`state` が `default`(0) かつ `expire` が未来のもの** だけを返します。
v0.1と同じ「テーブルの全行」が欲しい場合は `?state=all&include_expired=true` を付けてください。

```sh
curl 192.0.2.1:3000/leases/count                                # 実際に使われている数
curl '192.0.2.1:3000/leases/count?state=all&include_expired=true' # テーブルの全行数 (v0.1と同じ)
```

### 絞り込みパラメータ

`/leases` `/leases/count` `/leases6` `/leases6/count` で共通です。

| パラメータ | 既定 | 説明 |
| --- | --- | --- |
| `state` | `default` | `default` / `declined` / `expired-reclaimed` / `released` / `registered` / 数値 / `all`。名前はKeaの`lease_state`テーブルから読むので、Keaが値を増やしても使えます |
| `include_expired` | `false` | `true`にすると`expire`を過ぎたリースも含めます |
| `subnet_id` | - | サブネットID |
| `pool_id` | - | プールID (Keaスキーマ18以降) |
| `hwaddr` | - | MACアドレスの前方一致。`00:00:5e` `00-00-5e` `00005e` のどれでも可 |
| `duid` | - | DUIDの前方一致 (`/leases6`のみ) |
| `hostname` | - | ホスト名の部分一致 |
| `limit` / `offset` | - | ページング。`limit`は`max_limit`で頭打ちになります |
| `order_by` | `address` | `address` / `expire` / `hostname` / `subnet_id` / `state` |
| `desc` | `false` | `true`で降順 |
| `hwaddr_format` | `hex` | `hex`は`00005E005300`、`colon`は`00:00:5e:00:53:00` |

知らないパラメータを渡すと400を返します。`subnetid=1`のような綴り間違いが「絞ったつもりで全件」になるのを防ぐためです。

`/leases/count` と `/leases6/count` では `limit` / `offset` / `order_by` / `desc` は意味を持たないため無視されます (件数を数えるだけなので)。
また `expire` が `NULL` のリース (Keaスキーマ24以降でありえます) は期限切れ扱いになり、`include_expired=true` を付けたときだけ出てきます。

```sh
# サブネット1で、まだ生きているリース
curl '192.0.2.1:3000/leases?subnet_id=1'

# 特定ベンダのMACだけ (OUI前方一致)
curl '192.0.2.1:3000/leases?hwaddr=00:00:5e'

# ホスト名で探す
curl '192.0.2.1:3000/leases?hostname=test-pc'

# 期限が近い順に10件
curl '192.0.2.1:3000/leases?order_by=expire&limit=10'
```

### リース情報API

```sh
curl 192.0.2.1:3000/leases | jq
```
```json
[
  {
    "address": "192.0.2.100",
    "hwaddr": "00005E005300",
    "client_id": "0100005E005300",
    "valid_lifetime": 3600,
    "expire": "2026-03-31T07:49:01Z",
    "subnet_id": 1,
    "hostname": "test-pc01.example.com.",
    "cltt": "2026-03-31T06:49:01Z",
    "state": 0,
    "state_name": "default",
    "pool_id": 0,
    "fqdn_fwd": true,
    "fqdn_rev": true,
    "user_context": { "note": "example" },
    "relay_id": "0A0B0C",
    "remote_id": "DEADBEEF"
  }
]
```

先頭7項目とその並びはv0.1と同じです。以降が今回増えた項目です。

- `cltt` … クライアントが最後に通信した時刻。Keaはこの値をDBに持たないので `expire - valid_lifetime` から逆算しています
- `state` / `state_name` … リースの状態
- `user_context` … KeaがJSONテキストで持っている値。JSONとして解釈できればオブジェクトのまま返します
- `relay_id` / `remote_id` … Keaスキーマ16以降
- `pool_id` … Keaスキーマ18以降

接続先のKeaが古くて列が無い場合、その項目は `null` になります。起動時のログにどの列が無かったかを出します。

`expire` は常にUTCです。KeaはDHCPサーバのローカル時刻で書き込みますが、MySQLの`TIMESTAMP`型は内部的にUTCで保持されるため、DBサーバのタイムゾーン設定にかかわらず正しい時刻が返ります。

`/leases/{address}` は指定したアドレスのリースを1件だけ返します。こちらは`state`や有効期限で絞りません。

```sh
curl 192.0.2.1:3000/leases/192.0.2.100
curl 192.0.2.1:3000/leases6/2001:db8::100
```

### リース数API

```sh
curl 192.0.2.1:3000/leases/count
4
curl '192.0.2.1:3000/leases/count?subnet_id=1'
3
```
v0.1と同じく裸の数値を返します。上記の絞り込みパラメータがすべて使えます。

### IPv6

`/leases6` 系はIPv6リース(`lease6`テーブル)を返します。項目は `duid` `iaid` `prefix_len` `lease_type` など、IPv6固有のものになります。
`lease_type_name` (`IA_NA` / `IA_TA` / `IA_PD`) と `hwaddr_source_name` はKeaのコードテーブルから引いた名前です。

```sh
# 委任したプレフィックスだけ
curl '192.0.2.1:3000/leases6?state=all' | jq '.[] | select(.lease_type_name == "IA_PD")'
```

### 集計API

```sh
curl 192.0.2.1:3000/stats | jq
```
```json
{
  "schema_version": "35.0",
  "lease4": {
    "total": 9,
    "active": 4,
    "by_state": { "default": 5, "declined": 1, "expired-reclaimed": 1, "released": 1, "registered": 1 },
    "by_subnet": [
      { "subnet_id": 1, "total": 7, "active": 3, "by_state": { "default": 4, "declined": 1, "expired-reclaimed": 1, "released": 1 } }
    ]
  },
  "lease6": { "...": "同じ形式" }
}
```

`total`はテーブルの全行数、`active`は`default`かつ期限内の件数です。
全リースを引いてから数えるのではなく`GROUP BY`1本で済ませているので、リースが多い環境でも軽いです。

### メトリクス

`GET /metrics` でPrometheus形式の同じ集計が取れます。

```
# HELP kea_lease4_leases Number of rows in the Kea lease4 table.
# TYPE kea_lease4_leases gauge
kea_lease4_leases{subnet_id="1",state="default"} 4
kea_lease4_leases{subnet_id="1",state="expired-reclaimed"} 1
# HELP kea_lease4_active_leases Leases in the default state that have not expired yet.
# TYPE kea_lease4_active_leases gauge
kea_lease4_active_leases{subnet_id="1"} 3
```

```yaml
scrape_configs:
  - job_name: kea-lease-api
    static_configs:
      - targets: ['192.0.2.1:3000']
```

### ヘルスチェック

```sh
curl 192.0.2.1:3000/healthz
{"status":"ok","database":"ok","schema_version":"35.0"}
```
DBに実際にクエリを1本投げます。失敗した場合は503を返します。

### エラー応答

エラーはJSONで返ります。v0.1ではDBのエラー時にハンドラがパニックしていましたが、500応答になるよう直しました。

```sh
curl -i '192.0.2.1:3000/leases?state=bogus'
HTTP/1.1 400 Bad Request
{"error":"bad_request","message":"state に指定できるのは all, 数値, または default, declined, expired-reclaimed, released, registered です (指定値: bogus)"}
```

| ステータス | 意味 |
| --- | --- |
| 400 | パラメータが不正 |
| 404 | リースまたはエンドポイントが存在しない |
| 408 | `request_timeout_secs` を超えた |
| 500 | DBへの問い合わせに失敗 (詳細はサーバのログ) |
| 503 | `/healthz` でDBに繋がらない |

## 運用

`SIGTERM` / `SIGINT` を受けると、処理中のリクエストを捌き切ってから終了します。

systemdで動かす場合の例です。

```ini
[Unit]
Description=Kea lease API
After=network-online.target
Wants=network-online.target

[Service]
Type=exec
ExecStart=/usr/local/bin/kea_lease-db_api /etc/kea-lease-api/config.toml
Restart=on-failure
User=kea-lease-api
Group=kea-lease-api
# パスワードを設定ファイルに置かない場合
# EnvironmentFile=/etc/kea-lease-api/secret.env

NoNewPrivileges=yes
PrivateTmp=yes
ProtectSystem=strict
ProtectHome=yes
ProtectKernelTunables=yes
ProtectControlGroups=yes
RestrictAddressFamilies=AF_INET AF_INET6
MemoryDenyWriteExecute=yes

[Install]
WantedBy=multi-user.target
```

`contrib/kea-lease-api.service` に同じものを置いてあります。

このAPIには認証がありません。管理セグメントにだけ公開するか、リバースプロキシの後ろに置いてください。
プロキシの後ろに置く場合は `trust_proxy_header = true` にするとログに本来のクライアントIPが出ます (信頼できる経路でのみ有効にしてください)。

DBユーザにはリース関連テーブルへの`SELECT`権限だけあれば動きます。

## 対応バージョン

Keaのスキーマは起動時に`information_schema`を見て判定し、その環境にある列だけを読みます。
新しい列 (`state`, `pool_id`, `relay_id`, `remote_id` など) が無い古いスキーマでも動き、該当項目は`null`になります。

`lease6`のアドレス列はKeaスキーマ19.0で`VARCHAR(39)`の文字列から`BINARY(16)`のバイナリに変わっていますが、どちらも扱えます。

`state`名を引くための`lease_state`テーブルが無い場合も、Keaが定義している既知の値 (`default` / `declined` / `expired-reclaimed` / `released` / `registered`) で名前を補います。

動作確認はKeaスキーマ35.0とスキーマ1.0相当 (いずれもMariaDB 11) の両方で行っています。

# このプロジェクトについて
このプロジェクトはGoogle Gemini 3を使ってコーディングしました。
制作者の環境では問題なく動作しましたが、利用者の意図しない動作をする可能性があります。
このプロジェクトを使用したことで発生した如何なる事象も、制作者は一切の責任を負いません。
