# kea-lease-api

## 何ができるの？
ISC Kea DHCPサーバのIPv4アドレスリース情報を、バックエンドのMySQLもしくはMariaDBから取得して、JSON形式で返してくれるHTTP APIサーバが建てられます。

## 使い方
1. リポジトリをクローンしてビルドします
```sh
git clone https://github.com/panakuma/kea-lease-api.git
cd kea-lease-api
cargo build --release
```
`target/release` に実行ファイル( `kea_lease-db_api` )が生成されるので、適当なディレクトリにコピーします。

2. 設定ファイルの準備
実行ファイルと同じディレクトリに `config.toml.example` を `config.toml` としてコピーします。
適当なテキストエディタで開いて、DBへの接続情報を記入します。
任意でリッスンするIPやポート番号を変更します。

3. 実行
`kea_lease-db_api` をシェルから実行します
```sh
./kea_lease-db_api
```
プログラムが起動して、設定ファイルで指定したIP:ポート番号を開いて、HTTPリクエストを受けられる状態になります。

## API仕様
主に以下2つのAPIを用意しています。
- リース情報API
- リース数API

### リース情報APIについて
こちらはリースしたIPに関する情報が取得できるAPIです。
パス何も付けない or `/leases` を指定した際に応答します。
`curl 192.0.2.1:3000 | jq`とした場合のレスポンスは以下のようなフォーマットのJSONです。
```json
[
  {
    "address": "192.0.2.100",
    "hwaddr": "00005E005300",
    "client_id": "0100005E005300",
    "valid_lifetime": 3600,
    "expire": "2026-03-31T07:49:01Z",
    "subnet_id": 1,
    "hostname": "test-pc01.example.com."
  },
  {
    "address": "192.0.2.101",
    "hwaddr": "00005E005301",
    "client_id": "0100005E005301",
    "valid_lifetime": 3600,
    "expire": "2026-03-31T07:39:07Z",
    "subnet_id": 1,
    "hostname": "test-pc2"
  },
  {
    "address": "198.51.100.200",
    "hwaddr": "00005E005302",
    "client_id": "0100005E005302",
    "valid_lifetime": 3600,
    "expire": "2026-03-31T07:52:12Z",
    "subnet_id": 2,
    "hostname": "test-pc3"
  }
```

こちらのAPIは特にオプションは存在しません。
データベースの中身をすべてJSONで返します。

### リース数APIについて
こちらはリースしたIPの数が取得できるAPIです。
`/leases/count` パスを指定した際に応答します。
`curl 192.0.2.1:3000` とした場合のレスポンスは以下のような数字列です。
```sh
curl 192.0.2.1:3000/leases/count
3
```
また、このAPIはオプションが存在しています。
`subnet_id` を指定することで、keaの設定ファイルで指定した特定のsubnet\_idのリース数を応答します。
```sh
curl 192.0.2.1:3000/leases/count?subnet_id=1
2
```

# このプロジェクトについて
このプロジェクトはGoogle Gemini 3を使ってコーディングしました。
制作者の環境では問題なく動作しましたが、利用者の意図しない動作をする可能性があります。
このプロジェクトを使用したことで発生した如何なる事象も、制作者は一切の責任を負いません。
