//! 接続先の Kea スキーマで何が使えるか。
//!
//! Kea は版を重ねるごとに lease4 / lease6 の列を増やしてきた
//! (state は 3.0、user_context は 7.0、relay_id / remote_id は 16、
//!  pool_id は 18。lease6.address は 19.0 で VARCHAR(39) から BINARY(16) へ)。
//! そのため「この接続先に state 列はあるか」「address はバイナリか」という
//! 判断がコードのあちこちに現れる。以前はこれが SELECT リストの組み立て・
//! フィルタの可否・アドレスの復号・集計の GROUP BY に散っていて、対応列を
//! 1 つ増やすたびに 4 箇所を触る必要があった。
//!
//! ここに寄せて、他のモジュールは「使えるか」「どう書くか」を尋ねるだけにする。
//! 判定は起動時の 1 回で済むので、SELECT リストも先に組み立てておく。
//!
//! SELECT リストは Kea 1.0 からある列も含めて、全列を存在確認してから組み立てる。
//! information_schema で一部の列しか見えない環境では、該当項目が null になる
//! 代わりに、存在しない列を SELECT して落ちることはなくなる。
//! (テーブルの列が 1 つも見えない場合は「テーブルが無い」と判断して 404 を返す。
//!  `Schema::has_lease4` / `has_lease6` を参照。)

use crate::error::{ApiError, ApiResult};
use crate::schema::Schema;
use sqlx::mysql::{MySqlArguments, MySqlRow};
use sqlx::{Arguments as _, MySql, Row};
use std::collections::BTreeSet;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::Arc;

/// 対象テーブル。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    V4,
    V6,
}

impl Family {
    pub fn table(self) -> &'static str {
        match self {
            Family::V4 => "lease4",
            Family::V6 => "lease6",
        }
    }
}

/// SELECT リストへの載せ方。
#[derive(Debug, Clone, Copy)]
enum Column {
    /// そのまま取り出す。
    Plain(&'static str),
    /// HEX() でテキストにする。バイナリ列はそのままでは JSON にできない。
    Hex(&'static str),
}

impl Column {
    const fn name(self) -> &'static str {
        match self {
            Column::Plain(name) | Column::Hex(name) => name,
        }
    }

    fn select_expr(self, table: &str) -> String {
        match self {
            Column::Plain(name) => format!("{table}.{name}"),
            // HEX() は引数が NULL なら NULL を返すので、NULL 判定は不要。
            Column::Hex(name) => format!("HEX({table}.{name}) AS {name}"),
        }
    }
}

/// lease4 で読みたい列。address は符号化が別なので含めない。
const LEASE4_COLUMNS: &[Column] = &[
    Column::Hex("hwaddr"),
    Column::Hex("client_id"),
    Column::Plain("valid_lifetime"),
    Column::Plain("expire"),
    Column::Plain("subnet_id"),
    Column::Plain("hostname"),
    Column::Plain("state"),
    Column::Plain("pool_id"),
    Column::Plain("fqdn_fwd"),
    Column::Plain("fqdn_rev"),
    Column::Plain("user_context"),
    Column::Hex("relay_id"),
    Column::Hex("remote_id"),
];

/// lease6 で読みたい列。
const LEASE6_COLUMNS: &[Column] = &[
    Column::Hex("duid"),
    Column::Plain("valid_lifetime"),
    Column::Plain("expire"),
    Column::Plain("subnet_id"),
    Column::Plain("pref_lifetime"),
    Column::Plain("lease_type"),
    Column::Plain("iaid"),
    Column::Plain("prefix_len"),
    Column::Plain("hostname"),
    Column::Hex("hwaddr"),
    Column::Plain("hwtype"),
    Column::Plain("hwaddr_source"),
    Column::Plain("state"),
    Column::Plain("pool_id"),
    Column::Plain("fqdn_fwd"),
    Column::Plain("fqdn_rev"),
    Column::Plain("user_context"),
];

/// その列が Kea のどのスキーマバージョンで追加されたか。
/// (dhcpdb_create.mysql の ALTER TABLE を参照)
fn column_since(column: &str) -> Option<&'static str> {
    match column {
        "state" => Some("3.0"),
        "user_context" => Some("7.0"),
        "relay_id" | "remote_id" => Some("16"),
        "pool_id" => Some("18"),
        _ => None,
    }
}

/// address 列の持ち方。読み書きの両方をここで面倒みる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressCodec {
    /// lease4.address は UNSIGNED INT。
    V4,
    /// lease6.address が BINARY(16) (Kea スキーマ 19.0 以降)。
    V6Binary,
    /// lease6.address が VARCHAR(39) (18 以前)。
    V6Text,
}

/// アドレス 1 件を名指しするための WHERE 句と、そこに渡す値。
#[derive(Debug)]
pub struct AddressLookup {
    /// 正規化した表記。エラーメッセージ用。
    pub canonical: String,
    pub predicate: String,
    pub binds: Vec<Bind>,
}

impl AddressCodec {
    fn select_expr(self, table: &str) -> String {
        match self {
            AddressCodec::V4 => format!("INET_NTOA({table}.address) AS address"),
            // v6 はどちらの型でも生のまま取り出し、Rust 側で表記を整える。
            AddressCodec::V6Binary | AddressCodec::V6Text => format!("{table}.address"),
        }
    }

    fn decode(self, row: &MySqlRow) -> Option<String> {
        match self {
            // V4 は INET_NTOA() 済み、V6Text はもともとテキスト。
            AddressCodec::V4 | AddressCodec::V6Text => optional(row, "address"),
            AddressCodec::V6Binary => optional::<Vec<u8>>(row, "address").and_then(format_ipv6),
        }
    }

    fn lookup(self, table: &str, address: &str) -> ApiResult<AddressLookup> {
        match self {
            AddressCodec::V4 => {
                let parsed: Ipv4Addr = address.parse().map_err(|_| {
                    ApiError::bad_request(format!("IPv4 アドレスとして解釈できません: {address}"))
                })?;
                Ok(AddressLookup {
                    canonical: parsed.to_string(),
                    predicate: format!("{table}.address = ?"),
                    binds: vec![Bind::U32(u32::from(parsed))],
                })
            }
            AddressCodec::V6Binary => {
                let parsed = parse_ipv6(address)?;
                // 16 バイトを直接束縛すれば主キーがそのまま効き、
                // 表記ゆれ (2001:0db8:: と 2001:db8::) も吸収できる。
                Ok(AddressLookup {
                    canonical: parsed.to_string(),
                    predicate: format!("{table}.address = ?"),
                    binds: vec![Bind::Bytes(parsed.octets().to_vec())],
                })
            }
            AddressCodec::V6Text => {
                let parsed = parse_ipv6(address)?;
                // テキスト列では表記が揃っている保証がないので、
                // 正規化した表記と入力そのままの両方で突き合わせる。
                Ok(AddressLookup {
                    canonical: parsed.to_string(),
                    predicate: format!("{table}.address IN (?, ?)"),
                    binds: vec![
                        Bind::Str(parsed.to_string()),
                        Bind::Str(address.to_string()),
                    ],
                })
            }
        }
    }
}

fn parse_ipv6(address: &str) -> ApiResult<Ipv6Addr> {
    address
        .parse()
        .map_err(|_| ApiError::bad_request(format!("IPv6 アドレスとして解釈できません: {address}")))
}

/// BINARY(16) の 16 バイトを IPv6 アドレス表記にする。
fn format_ipv6(bytes: Vec<u8>) -> Option<String> {
    let octets: [u8; 16] = bytes.try_into().ok()?;
    Some(Ipv6Addr::from(octets).to_string())
}

/// 接続先スキーマで、この系統 (v4 / v6) に何ができるか。
///
/// 起動時に 1 度だけ組み立てて `Arc` で共有する。
#[derive(Debug, Clone)]
pub struct LeaseCapability {
    family: Family,
    schema: Arc<Schema>,
    present: bool,
    columns: BTreeSet<&'static str>,
    address_codec: AddressCodec,
    select_list: String,
    order_columns: Vec<&'static str>,
}

impl LeaseCapability {
    pub fn detect(family: Family, schema: Arc<Schema>) -> Self {
        let (present, catalogue): (bool, &[Column]) = match family {
            Family::V4 => (schema.has_lease4(), LEASE4_COLUMNS),
            Family::V6 => (schema.has_lease6(), LEASE6_COLUMNS),
        };
        let address_codec = match family {
            Family::V4 => AddressCodec::V4,
            Family::V6 if schema.lease6_address_is_binary() => AddressCodec::V6Binary,
            Family::V6 => AddressCodec::V6Text,
        };

        let table = family.table();
        let mut columns = BTreeSet::new();
        let mut select = vec![address_codec.select_expr(table)];
        for column in catalogue.iter().copied() {
            let name = column.name();
            let exists = match family {
                Family::V4 => schema.lease4_has(name),
                Family::V6 => schema.lease6_has(name),
            };
            if exists {
                columns.insert(name);
                select.push(column.select_expr(table));
            }
        }

        // address は常にあるので無条件。残りは接続先スキーマに列がある場合だけ
        // 候補に出す。無い列を ORDER BY に書くとクエリごと落ちる。
        let mut order_columns = vec!["address"];
        for candidate in ["expire", "hostname", "subnet_id", "state"] {
            if columns.contains(candidate) {
                order_columns.push(candidate);
            }
        }

        Self {
            family,
            schema,
            present,
            columns,
            address_codec,
            select_list: select.join(", "),
            order_columns,
        }
    }

    pub fn family(&self) -> Family {
        self.family
    }

    pub fn table(&self) -> &'static str {
        self.family.table()
    }

    /// テーブル自体が存在するか。
    pub fn present(&self) -> bool {
        self.present
    }

    /// この列を SELECT リストに載せているか。
    ///
    /// 対象は上のカタログに並べた列だけで、address は含まない。
    /// address は必ず存在し、読み方は `AddressCodec` が持っている。
    pub fn has(&self, column: &str) -> bool {
        self.columns.contains(column)
    }

    /// 列が無ければ 400。どの Kea スキーマから使えるかを添える。
    pub fn require_column(&self, column: &str) -> ApiResult<()> {
        if self.has(column) {
            return Ok(());
        }
        let table = self.table();
        Err(ApiError::bad_request(match column_since(column) {
            Some(since) => format!(
                "接続先の {table} テーブルには {column} 列がありません (Kea スキーマ {since} 以降が必要です)"
            ),
            None => format!("接続先の {table} テーブルには {column} 列がありません"),
        }))
    }

    pub fn select_list(&self) -> &str {
        &self.select_list
    }

    /// order_by に指定できる列。
    pub fn order_columns(&self) -> &[&'static str] {
        &self.order_columns
    }

    /// state 名などのコードテーブル。
    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    /// 行から address を読む。
    pub fn address(&self, row: &MySqlRow) -> Option<String> {
        self.address_codec.decode(row)
    }

    /// アドレス 1 件を名指しする WHERE 句を組み立てる。
    pub fn address_lookup(&self, address: &str) -> ApiResult<AddressLookup> {
        self.address_codec.lookup(self.table(), address)
    }
}

/// 列があれば読み、無ければ None。
///
/// SELECT リストは接続先スキーマに合わせて組み立てているので
/// `ColumnNotFound` は想定内。デコードに失敗した場合も、1 件のために
/// 応答全体を落とすより null で返したほうが実用的なのでログに残して続行する。
pub fn optional<'r, T>(row: &'r MySqlRow, name: &str) -> Option<T>
where
    T: sqlx::Decode<'r, MySql> + sqlx::Type<MySql>,
{
    match row.try_get::<Option<T>, _>(name) {
        Ok(value) => value,
        Err(sqlx::Error::ColumnNotFound(_)) => None,
        Err(error) => {
            tracing::warn!("列 {name} を読めませんでした: {error}");
            None
        }
    }
}

/// バインドする値。型ごとに持たないと sqlx に渡せない。
#[derive(Debug, Clone)]
pub enum Bind {
    U32(u32),
    Str(String),
    Bytes(Vec<u8>),
}

/// バインド値を sqlx の引数列に詰める。
pub fn arguments(binds: &[Bind]) -> Result<MySqlArguments, sqlx::Error> {
    let mut args = MySqlArguments::default();
    for bind in binds {
        let result = match bind {
            Bind::U32(value) => args.add(*value),
            Bind::Str(value) => args.add(value.as_str()),
            Bind::Bytes(value) => args.add(value.as_slice()),
        };
        result.map_err(|error| {
            sqlx::Error::Encode(format!("クエリ引数を組み立てられませんでした: {error}").into())
        })?;
    }
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capability(
        family: Family,
        lease4: &[(&str, &str)],
        lease6: &[(&str, &str)],
    ) -> LeaseCapability {
        LeaseCapability::detect(family, Arc::new(Schema::from_columns(lease4, lease6)))
    }

    /// Kea 1.0 相当。後から追加された列は SELECT リストに載らない。
    #[test]
    fn old_schema_selects_only_existing_columns() {
        let capability = capability(
            Family::V4,
            &[
                ("address", "int"),
                ("hwaddr", "varbinary"),
                ("valid_lifetime", "int"),
                ("expire", "timestamp"),
                ("subnet_id", "int"),
            ],
            &[],
        );
        assert_eq!(
            capability.select_list(),
            "INET_NTOA(lease4.address) AS address, HEX(lease4.hwaddr) AS hwaddr, \
             lease4.valid_lifetime, lease4.expire, lease4.subnet_id"
        );
        assert!(!capability.has("state"));
        // hostname 列が無いスキーマなので、order_by の候補にも出さない。
        assert_eq!(
            capability.order_columns(),
            ["address", "expire", "subnet_id"]
        );
    }

    #[test]
    fn state_column_unlocks_the_state_ordering() {
        let capability = capability(Family::V4, &[("state", "int")], &[]);
        assert!(capability.has("state"));
        assert!(capability.order_columns().contains(&"state"));
        assert!(capability.require_column("state").is_ok());
    }

    /// lease6 も同じ判定を通る。hostname 列が無ければ order_by の候補に出さない。
    #[test]
    fn lease6_order_columns_follow_the_schema_too() {
        let old = capability(
            Family::V6,
            &[],
            &[
                ("address", "varchar"),
                ("duid", "varbinary"),
                ("expire", "timestamp"),
            ],
        );
        assert_eq!(old.order_columns(), ["address", "expire"]);

        let modern = capability(
            Family::V6,
            &[],
            &[
                ("address", "binary"),
                ("expire", "timestamp"),
                ("hostname", "varchar"),
                ("subnet_id", "int"),
                ("state", "int"),
            ],
        );
        assert_eq!(
            modern.order_columns(),
            ["address", "expire", "hostname", "subnet_id", "state"]
        );
    }

    #[test]
    fn missing_column_names_the_required_schema_version() {
        let capability = capability(Family::V4, &[("address", "int")], &[]);
        let error = capability.require_column("pool_id").unwrap_err();
        assert!(format!("{error:?}").contains("18"));
    }

    /// スキーマ 19.0 以降は BINARY(16)。主キーへ 16 バイトを直接束縛する。
    #[test]
    fn binary_lease6_address_binds_raw_octets() {
        let capability = capability(Family::V6, &[], &[("address", "binary")]);
        let lookup = capability.address_lookup("2001:0db8::1").unwrap();
        assert_eq!(lookup.canonical, "2001:db8::1");
        assert_eq!(lookup.predicate, "lease6.address = ?");
        assert!(matches!(lookup.binds.as_slice(), [Bind::Bytes(bytes)] if bytes.len() == 16));
    }

    /// 18 以前は VARCHAR(39)。表記ゆれを拾うため 2 通りで突き合わせる。
    #[test]
    fn text_lease6_address_matches_both_spellings() {
        let capability = capability(Family::V6, &[], &[("address", "varchar")]);
        let lookup = capability.address_lookup("2001:0db8::1").unwrap();
        assert_eq!(lookup.predicate, "lease6.address IN (?, ?)");
        assert!(
            matches!(lookup.binds.as_slice(), [Bind::Str(canonical), Bind::Str(raw)]
                if canonical == "2001:db8::1" && raw == "2001:0db8::1")
        );
    }

    #[test]
    fn bad_address_is_rejected() {
        let v4 = capability(Family::V4, &[("address", "int")], &[]);
        assert!(v4.address_lookup("not-an-address").is_err());
        let v6 = capability(Family::V6, &[], &[("address", "binary")]);
        assert!(v6.address_lookup("192.0.2.1").is_err());
    }

    #[test]
    fn binary_address_is_formatted_as_ipv6() {
        let mut octets = vec![0u8; 16];
        octets[0] = 0x20;
        octets[1] = 0x01;
        octets[2] = 0x0d;
        octets[3] = 0xb8;
        octets[15] = 0x01;
        assert_eq!(format_ipv6(octets).as_deref(), Some("2001:db8::1"));
    }

    #[test]
    fn binary_address_needs_16_bytes() {
        assert!(format_ipv6(vec![0u8; 4]).is_none());
    }
}
