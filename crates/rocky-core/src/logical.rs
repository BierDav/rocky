use crate::{
    AppId, ApplicationManifest, AuthorLogScope, Commitment, DefinitionItem, DomainId, DomainRule,
    SqlValue, commitment, definition_item_id,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const LOGICAL_SCHEMA_FORMAT: u32 = 7;
pub const TYPED_RULE: &str = "rocky.sdk.typed.v2";
pub const MAX_LOGICAL_SCHEMA_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Affinity {
    Integer,
    Real,
    Text,
    Blob,
    Numeric,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Collation {
    Binary = 0,
    Nocase = 1,
    Rtrim = 2,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TableClass {
    LocalOnly,
    Replayed,
}
impl TableClass {
    pub fn typed(self) -> bool {
        self == Self::Replayed
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColumnClass {
    Replicated,
    LocalOnly,
    Derived { rule: String },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchemaSource {
    pub kind: String,
    pub version: u32,
    pub identity: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogicalColumn {
    pub sql_name: String,
    pub affinity: Affinity,
    pub not_null: bool,
    pub default: Option<String>,
    pub collation: Collation,
    pub class: ColumnClass,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexColumn {
    pub name: String,
    pub descending: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogicalIndex {
    pub name: String,
    pub unique: bool,
    pub columns: Vec<IndexColumn>,
    pub create_sql: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expressions: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predicate: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogicalForeignKey {
    pub table: String,
    pub columns: Vec<String>,
    pub references: Vec<String>,
    pub on_delete: String,
    pub on_update: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deferred: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogicalView {
    pub name: String,
    pub create_sql: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogicalAssertion {
    pub name: String,
    pub select_sql: String,
}

impl LogicalAssertion {
    pub fn tables(&self) -> Vec<String> {
        match crate::deterministic::assertion_tables(&self.select_sql) {
            crate::deterministic::AssertionShape::Reads(tables) => tables
                .into_iter()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
            _ => Vec::new(),
        }
    }
    pub fn reads_table(&self, table: &str) -> bool {
        self.tables()
            .iter()
            .any(|name| name.eq_ignore_ascii_case(table))
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogicalRestriction {
    pub name: String,
    pub table: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
    pub refuse_update: bool,
    pub refuse_delete: bool,
}
impl LogicalRestriction {
    pub fn immutable(
        name: impl Into<String>,
        table: impl Into<String>,
        when: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            table: table.into(),
            when: Some(when.into()),
            refuse_update: true,
            refuse_delete: true,
        }
    }
    pub fn append_only(name: impl Into<String>, table: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            table: table.into(),
            when: None,
            refuse_update: true,
            refuse_delete: true,
        }
    }
    pub fn bind_when(
        &self,
        table: &LogicalTable,
        bind: impl Fn(&LogicalColumn) -> String,
    ) -> Option<String> {
        let Some(when) = &self.when else {
            return Some("1".into());
        };
        bind_rule(when, |name| {
            table
                .column(name)
                .filter(|column| column.class != ColumnClass::LocalOnly)
                .map(&bind)
        })
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogicalTable {
    pub sql_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub granted_like: Option<String>,
    pub class: TableClass,
    pub create_sql: String,
    pub auto_generate: bool,
    pub columns: Vec<LogicalColumn>,
    pub primary_key: Vec<String>,
    pub partition_by: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition_group: Option<String>,
    pub indexes: Vec<LogicalIndex>,
    pub foreign_keys: Vec<LogicalForeignKey>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogicalSchema {
    pub format: u32,
    pub publisher: [u8; 32],
    pub application: String,
    pub source: SchemaSource,
    pub tables: Vec<LogicalTable>,
    pub views: Vec<LogicalView>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assertions: Vec<LogicalAssertion>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub restrictions: Vec<LogicalRestriction>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Incompatibility {
    ForeignKey,
    Function(String),
    Arithmetic(char),
    Reference(String),
    CrossScopeReference(String),
    ClassReference(String),
    Assertion(String),
    Restriction(String),
    Unsupported(TableClass),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    Trusted,
    Secure,
}
impl Profile {
    fn replays_intents(self) -> bool {
        matches!(self, Self::Secure)
    }
}
const TWO_POW_63: f64 = -(i64::MIN as f64);
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SchemaError {
    Invalid(String),
    Incompatible {
        table: String,
        reason: Incompatibility,
    },
}
impl std::fmt::Display for SchemaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(reason) => write!(f, "invalid logical schema: {reason}"),
            Self::Incompatible { table, reason } => {
                write!(f, "{table} cannot be installed: {reason:?}")
            }
        }
    }
}
impl std::error::Error for SchemaError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueError {
    Null,
    Class(Affinity),
    Precision,
    Arity,
    KeyTooLong,
}
impl std::fmt::Display for ValueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for ValueError {}

impl Affinity {
    pub fn of_declared(declared: &str) -> Self {
        let declared = declared.to_ascii_uppercase();
        let has = |part: &str| declared.contains(part);
        if has("INT") {
            Self::Integer
        } else if has("CHAR") || has("CLOB") || has("TEXT") {
            Self::Text
        } else if has("BLOB") || declared.trim().is_empty() {
            Self::Blob
        } else if has("REAL") || has("FLOA") || has("DOUB") {
            Self::Real
        } else {
            Self::Numeric
        }
    }
    pub fn store(self, value: SqlValue) -> Result<SqlValue, ValueError> {
        const EXACT: i64 = 1 << 53;
        match (self, value) {
            (_, SqlValue::Null) => Ok(SqlValue::Null),
            (Self::Integer, value @ SqlValue::Integer(_))
            | (Self::Real, value @ SqlValue::Real(_))
            | (Self::Text, value @ SqlValue::Text(_))
            | (Self::Blob, value @ SqlValue::Blob(_)) => Ok(value),
            (Self::Integer, SqlValue::Real(real)) => {
                let real = real.get();
                if real.fract() == 0.0 && (-TWO_POW_63..TWO_POW_63).contains(&real) {
                    Ok(SqlValue::Integer(real as i64))
                } else {
                    Err(ValueError::Class(self))
                }
            }
            (Self::Real, SqlValue::Integer(integer)) if (-EXACT..=EXACT).contains(&integer) => {
                SqlValue::real(integer as f64).ok_or(ValueError::Precision)
            }
            (Self::Real, SqlValue::Integer(_)) => Err(ValueError::Precision),
            _ => Err(ValueError::Class(self)),
        }
    }
}
impl Collation {
    pub fn named(name: &str) -> Option<Self> {
        match name.to_ascii_uppercase().as_str() {
            "BINARY" => Some(Self::Binary),
            "NOCASE" => Some(Self::Nocase),
            "RTRIM" => Some(Self::Rtrim),
            _ => None,
        }
    }
    pub fn fold(self, text: &str) -> Vec<u8> {
        match self {
            Self::Binary => text.as_bytes().to_vec(),
            Self::Nocase => text.bytes().map(|byte| byte.to_ascii_lowercase()).collect(),
            Self::Rtrim => text.trim_end_matches(' ').as_bytes().to_vec(),
        }
    }
}

pub fn identifier(name: &str) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        && !bytes[0].is_ascii_digit()
}
fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}
pub fn foreign_key_clause(key: &LogicalForeignKey) -> String {
    let deferred = if key.deferred {
        " DEFERRABLE INITIALLY DEFERRED"
    } else {
        ""
    };
    format!(
        "FOREIGN KEY({}) REFERENCES {}({}) ON DELETE {}{deferred}",
        key.columns
            .iter()
            .map(|name| quote(name))
            .collect::<Vec<_>>()
            .join(","),
        quote(&key.table),
        key.references
            .iter()
            .map(|name| quote(name))
            .collect::<Vec<_>>()
            .join(","),
        key.on_delete
    )
}
pub const DELETE_CASCADE: &str = "CASCADE";
pub const DELETE_SET_NULL: &str = "SET NULL";
pub const DELETE_TRIGGERS: &str = "rocky_set_null_";
fn reserved(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.starts_with("rocky_") || name.starts_with("sqlite_") || name == "excluded"
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Token {
    Word(String),
    Quoted(String),
    Literal,
    Symbol(char),
}
fn spanned(sql: &str) -> Option<Vec<(Token, std::ops::Range<usize>)>> {
    let mut chars = sql.char_indices().peekable();
    let mut tokens = Vec::new();
    while let Some((start, first)) = chars.next() {
        if first.is_whitespace() {
            continue;
        }
        let token = if ['\'', '"', '`', '['].contains(&first) {
            let end = if first == '[' { ']' } else { first };
            let mut content = String::new();
            loop {
                let (_, next) = chars.next()?;
                if next == end {
                    if end != ']' && chars.peek().is_some_and(|(_, next)| *next == end) {
                        chars.next();
                    } else {
                        break;
                    }
                }
                content.push(next);
            }
            if first == '\'' {
                Token::Literal
            } else {
                Token::Quoted(content)
            }
        } else if first.is_ascii_alphanumeric() || first == '_' {
            let mut word = String::from(first);
            while let Some((_, next)) = chars
                .peek()
                .filter(|(_, next)| next.is_ascii_alphanumeric() || *next == '_' || *next == '.')
            {
                let dot_after_name =
                    *next == '.' && !word.bytes().all(|byte| byte.is_ascii_digit());
                if dot_after_name {
                    break;
                }
                word.push(*next);
                chars.next();
            }
            if first.is_ascii_digit() {
                Token::Literal
            } else {
                Token::Word(word)
            }
        } else {
            Token::Symbol(first)
        };
        let end = chars.peek().map_or(sql.len(), |(end, _)| *end);
        tokens.push((token, start..end));
    }
    Some(tokens)
}
pub(crate) fn tokenize(sql: &str) -> Option<Vec<Token>> {
    Some(spanned(sql)?.into_iter().map(|(token, _)| token).collect())
}
pub(crate) fn keyword(token: &Token, expected: &str) -> bool {
    matches!(token, Token::Word(word) if word.eq_ignore_ascii_case(expected))
}
pub fn declared_collations(create_sql: &str) -> Vec<(String, String)> {
    let tokens = tokenize(create_sql).unwrap_or_default();
    let (mut depth, mut column, mut collations) = (0usize, None::<String>, Vec::new());
    for (index, token) in tokens.iter().enumerate() {
        match token {
            Token::Symbol('(') => depth += 1,
            Token::Symbol(')') => depth = depth.saturating_sub(1),
            _ => {}
        }
        if depth != 1 {
            continue;
        }
        let starts = matches!(
            tokens.get(index.wrapping_sub(1)),
            Some(Token::Symbol('(' | ','))
        );
        match token {
            Token::Quoted(name) if starts => column = Some(name.clone()),
            Token::Word(name) if starts => {
                let constraint = ["CONSTRAINT", "PRIMARY", "UNIQUE", "CHECK", "FOREIGN"]
                    .iter()
                    .any(|word| name.eq_ignore_ascii_case(word));
                column = (!constraint).then(|| name.clone());
            }
            token if keyword(token, "COLLATE") => {
                if let (Some(column), Some(Token::Word(name) | Token::Quoted(name))) =
                    (&column, tokens.get(index + 1))
                {
                    collations.push((column.clone(), name.clone()));
                }
            }
            _ => {}
        }
    }
    collations
}
const RULE_FUNCTIONS: &[&str] = &[
    "abs",
    "checked_add",
    "checked_div",
    "checked_mul",
    "checked_sub",
    "coalesce",
    "hex",
    "ifnull",
    "instr",
    "length",
    "lower",
    "ltrim",
    "nullif",
    "replace",
    "round",
    "rtrim",
    "substr",
    "substring",
    "trim",
    "upper",
];
const RULE_WORDS: &[&str] = &[
    "AND", "CASE", "ELSE", "END", "IS", "NOT", "NULL", "OR", "THEN", "WHEN",
];
fn rule_reference(
    tokens: &[(Token, std::ops::Range<usize>)],
    index: usize,
) -> Option<Option<&str>> {
    match &tokens[index].0 {
        Token::Literal => Some(None),
        Token::Quoted(name) => Some(Some(name)),
        Token::Symbol(symbol) => "(),+-*/%|<>=!".contains(*symbol).then_some(None),
        Token::Word(word) => {
            let call = matches!(tokens.get(index + 1), Some((Token::Symbol('('), _)));
            if call {
                RULE_FUNCTIONS
                    .contains(&word.to_ascii_lowercase().as_str())
                    .then_some(None)
            } else if RULE_WORDS.contains(&word.to_ascii_uppercase().as_str()) {
                Some(None)
            } else {
                Some(Some(word))
            }
        }
    }
}
const MAX_RULE_BYTES: usize = 512;

pub fn bind_rule(rule: &str, bind: impl Fn(&str) -> Option<String>) -> Option<String> {
    let tokens =
        spanned(rule).filter(|tokens| !tokens.is_empty() && rule.len() <= MAX_RULE_BYTES)?;
    let (mut bound, mut copied) = (String::new(), 0);
    for (index, (_, span)) in tokens.iter().enumerate() {
        if let Some(column) = rule_reference(&tokens, index)? {
            bound.push_str(&rule[copied..span.start]);
            bound.push_str(&bind(column)?);
            copied = span.end;
        }
    }
    bound.push_str(&rule[copied..]);
    Some(bound)
}
pub(crate) fn single_expression(sql: &str) -> bool {
    let Some(tokens) = tokenize(sql).filter(|tokens| !tokens.is_empty()) else {
        return false;
    };
    let mut depth = 0usize;
    for token in &tokens {
        match token {
            Token::Symbol('(') => depth += 1,
            Token::Symbol(')') if depth == 0 => return false,
            Token::Symbol(')') => depth -= 1,
            Token::Symbol(';') => return false,
            token if keyword(token, "SELECT") => return false,
            _ => {}
        }
    }
    depth == 0
}
pub fn derived_rule(rule: &str, columns: &[&str]) -> bool {
    bind_rule(rule, |column| columns.contains(&column).then(String::new)).is_some()
}

fn invalid<T>(reason: impl Into<String>) -> Result<T, SchemaError> {
    Err(SchemaError::Invalid(reason.into()))
}
impl LogicalTable {
    pub fn foreign_key_sql(&self, key: &LogicalForeignKey) -> String {
        if key.on_delete == DELETE_SET_NULL && !self.nulls_whole(key) {
            return foreign_key_clause(&LogicalForeignKey {
                on_delete: "NO ACTION".into(),
                ..key.clone()
            });
        }
        foreign_key_clause(key)
    }
    fn nulls_whole(&self, key: &LogicalForeignKey) -> bool {
        key.columns
            .iter()
            .all(|name| self.column(name).is_some_and(|column| !column.not_null))
    }
    pub fn identity_columns(&self) -> Vec<String> {
        let mut columns = self.partition_by.clone();
        if !columns.iter().any(|name| name.eq_ignore_ascii_case("id")) {
            columns.push("id".into());
        }
        columns
    }
    pub fn is_identity_index(&self, index: &LogicalIndex) -> bool {
        index.unique
            && index.expressions.is_empty()
            && index.predicate.is_none()
            && index.columns.iter().all(|column| !column.descending)
            && index
                .columns
                .iter()
                .map(|column| &column.name)
                .zip(self.identity_columns())
                .all(|(column, identity)| column.eq_ignore_ascii_case(&identity))
            && index.columns.len() == self.identity_columns().len()
    }

    pub fn column(&self, sql_name: &str) -> Option<&LogicalColumn> {
        self.columns
            .iter()
            .find(|column| column.sql_name.eq_ignore_ascii_case(sql_name))
    }
    pub fn key(&self) -> impl Iterator<Item = &LogicalColumn> {
        self.primary_key.iter().filter_map(|name| self.column(name))
    }
    pub fn fields(&self) -> impl Iterator<Item = &LogicalColumn> {
        self.columns.iter().filter(|column| {
            column.class == ColumnClass::Replicated
                && !self
                    .primary_key
                    .iter()
                    .any(|name| name.eq_ignore_ascii_case(&column.sql_name))
        })
    }
    fn validate(&self) -> Result<(), SchemaError> {
        let name = &self.sql_name;
        if !identifier(name) || reserved(name) {
            return invalid(format!("table name {name}"));
        }
        let statement = tokenize(&self.create_sql).unwrap_or_default();
        let word = |index: usize, expected: &str| matches!(statement.get(index), Some(Token::Word(word)) if word.eq_ignore_ascii_case(expected));
        if !word(0, "CREATE") || !word(1, "TABLE") || statement.contains(&Token::Symbol(';')) {
            return invalid(format!("{name} needs one CREATE TABLE statement"));
        }
        if self.columns.is_empty() || self.columns.len() > 2000 {
            return invalid(format!("{name} column count"));
        }
        let mut names = BTreeSet::new();
        for column in &self.columns {
            if !identifier(&column.sql_name) || !names.insert(column.sql_name.to_ascii_lowercase())
            {
                return invalid(format!("{name} column {}", column.sql_name));
            }
        }
        let mut key = BTreeSet::new();
        if self.primary_key.len() > usize::from(u8::MAX)
            || self.primary_key.iter().any(|column| {
                !key.insert(column.to_ascii_lowercase())
                    || self
                        .column(column)
                        .is_none_or(|column| column.class != ColumnClass::Replicated)
            })
        {
            return invalid(format!("{name} primary key"));
        }
        for index in &self.indexes {
            let statement = tokenize(&index.create_sql).unwrap_or_default();
            let create = matches!(statement.first(), Some(Token::Word(word)) if word.eq_ignore_ascii_case("CREATE"));
            if !identifier(&index.name)
                || reserved(&index.name)
                || !create
                || statement.contains(&Token::Symbol(';'))
                || index
                    .columns
                    .iter()
                    .any(|column| self.column(&column.name).is_none())
            {
                return invalid(format!("{name} index {}", index.name));
            }
        }
        for key in &self.foreign_keys {
            if key.columns.is_empty() || key.columns.len() != key.references.len() {
                return invalid(format!("{name} foreign key"));
            }
            for column in &key.columns {
                match self.column(column).map(|held| &held.class) {
                    None => return invalid(format!("{name} column {column}")),
                    Some(ColumnClass::LocalOnly) => {
                        return invalid(format!(
                            "{name}.{column} is local-only and cannot hold a foreign key"
                        ));
                    }
                    Some(ColumnClass::Derived { .. }) if self.class == TableClass::Replayed => {
                        return invalid(format!(
                            "{name}.{column} is derived and cannot hold a foreign key"
                        ));
                    }
                    Some(_) => {}
                }
            }
        }
        Ok(())
    }
    fn installable(&self) -> Result<(), Incompatibility> {
        let statement = tokenize(&self.create_sql).unwrap_or_default();
        let references = statement
            .iter()
            .filter(|token| keyword(token, "REFERENCES"))
            .count();
        if !self.class.typed() {
            if !self.foreign_keys.is_empty() || references > 0 {
                return Err(Incompatibility::ForeignKey);
            }
            return Ok(());
        }
        if references != self.foreign_keys.len() {
            return Err(Incompatibility::ForeignKey);
        }
        for key in &self.foreign_keys {
            let clause = self.foreign_key_sql(key);
            if ![DELETE_CASCADE, DELETE_SET_NULL].contains(&key.on_delete.as_str())
                || key.on_update != "NO ACTION"
                || !(self.create_sql.contains(&format!("{clause},"))
                    || self.create_sql.contains(&format!("{clause})")))
            {
                return Err(Incompatibility::Reference(
                    key.columns.first().cloned().unwrap_or_default(),
                ));
            }
        }
        if let Some(function) = std::iter::once(&self.create_sql)
            .chain(self.indexes.iter().map(|index| &index.create_sql))
            .find_map(|sql| crate::deterministic::refused_function(sql))
        {
            return Err(Incompatibility::Function(function));
        }
        if let Some(operator) = std::iter::once(&self.create_sql)
            .chain(self.indexes.iter().map(|index| &index.create_sql))
            .find_map(|sql| crate::deterministic::refused_operator(sql))
        {
            return Err(Incompatibility::Arithmetic(operator));
        }
        Ok(())
    }
}

#[derive(Serialize)]
struct ReplicatedColumn<'a> {
    name: &'a str,
    affinity: Affinity,
    not_null: bool,
    collation: Collation,
    default: Option<&'a str>,
}
#[derive(Serialize, PartialEq, Eq, PartialOrd, Ord)]
struct ReplicatedReference<'a> {
    columns: Vec<&'a str>,
    table: &'a str,
    on_delete: &'a str,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    deferred: bool,
}
#[derive(Serialize)]
struct ReplicatedTable<'a> {
    class: TableClass,
    name: &'a str,
    granted_like: Option<&'a str>,
    key: Vec<ReplicatedColumn<'a>>,
    fields: Vec<ReplicatedColumn<'a>>,
    references: Vec<ReplicatedReference<'a>>,
    partition_by: Vec<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    partition_group: Option<&'a str>,
    replayed: ReplayedConstraints<'a>,
}
#[derive(Serialize)]
struct ReplayedConstraints<'a> {
    create_sql: &'a str,
    indexes: Vec<&'a str>,
    assertions: Vec<&'a LogicalAssertion>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    restrictions: Vec<&'a LogicalRestriction>,
}
#[derive(Serialize)]
struct Replicated<'a> {
    format: u32,
    tables: Vec<ReplicatedTable<'a>>,
}
fn replicated_column(column: &LogicalColumn) -> ReplicatedColumn<'_> {
    ReplicatedColumn {
        name: &column.sql_name,
        affinity: column.affinity,
        not_null: column.not_null,
        collation: column.collation,
        default: column.default.as_deref(),
    }
}
fn replicated_table<'a>(schema: &'a LogicalSchema, table: &'a LogicalTable) -> ReplicatedTable<'a> {
    let mut fields: Vec<_> = table.fields().map(replicated_column).collect();
    fields.sort_by_key(|column| column.name);
    let name = |name: &str| {
        table
            .column(name)
            .map_or_else(|| "", |column| column.sql_name.as_str())
    };
    let mut references: Vec<_> = table
        .foreign_keys
        .iter()
        .map(|key| ReplicatedReference {
            columns: key.columns.iter().map(|column| name(column)).collect(),
            table: schema
                .table(&key.table)
                .map_or(key.table.as_str(), |parent| parent.sql_name.as_str()),
            on_delete: &key.on_delete,
            deferred: key.deferred,
        })
        .collect();
    references.sort();
    let mut indexes: Vec<_> = table
        .indexes
        .iter()
        .map(|index| index.create_sql.as_str())
        .collect();
    indexes.sort_unstable();
    ReplicatedTable {
        class: table.class,
        name: &table.sql_name,
        granted_like: table.granted_like.as_deref(),
        key: table.key().map(replicated_column).collect(),
        fields,
        references,
        partition_by: table
            .partition_by
            .iter()
            .filter_map(|name| table.column(name).map(|column| column.sql_name.as_str()))
            .collect(),
        partition_group: table.partition_group.as_deref(),
        replayed: ReplayedConstraints {
            create_sql: &table.create_sql,
            indexes,
            assertions: schema
                .assertions
                .iter()
                .filter(|assertion| assertion.reads_table(&table.sql_name))
                .collect(),
            restrictions: schema.restrictions_of(table).collect(),
        },
    }
}
fn json(value: &impl Serialize) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

impl LogicalSchema {
    pub fn delete_triggers(&self, installed: impl Fn(&str) -> bool) -> Vec<String> {
        let mut triggers = Vec::new();
        for table in self
            .tables
            .iter()
            .filter(|table| installed(&table.sql_name))
        {
            for (index, key) in table.foreign_keys.iter().enumerate() {
                if key.on_delete != DELETE_SET_NULL
                    || table.nulls_whole(key)
                    || !installed(&key.table)
                {
                    continue;
                }
                let nulled: Vec<_> = key
                    .columns
                    .iter()
                    .filter(|name| table.column(name).is_some_and(|column| !column.not_null))
                    .map(|name| format!("{}=NULL", quote(name)))
                    .collect();
                let matched: Vec<_> = key
                    .columns
                    .iter()
                    .zip(&key.references)
                    .map(|(column, parent)| format!("{}=OLD.{}", quote(column), quote(parent)))
                    .collect();
                triggers.push(format!(
                    "CREATE TRIGGER {} AFTER DELETE ON {} FOR EACH ROW BEGIN UPDATE {} SET {} WHERE {}; END",
                    quote(&format!("{DELETE_TRIGGERS}{}_{index}", table.sql_name)),
                    quote(&key.table),
                    quote(&table.sql_name),
                    nulled.join(","),
                    matched.join(" AND "),
                ));
            }
        }
        triggers
    }
    pub fn app_id(&self) -> AppId {
        ApplicationManifest::app_id(self.publisher, &self.application)
    }
    pub fn validate(&self) -> Result<(), SchemaError> {
        self.require_row_identity()?;
        if self.application.is_empty() || self.application.len() > 128 {
            return invalid("application name");
        }
        if self.source.kind.is_empty() || self.tables.is_empty() {
            return invalid("source or tables missing");
        }
        if !self.tables.is_sorted_by(|a, b| a.sql_name < b.sql_name) {
            return invalid("tables are not sorted by unique name");
        }
        self.partition_groups()?;
        let mut names = BTreeSet::new();
        for table in &self.tables {
            table.validate()?;
            for key in &table.foreign_keys {
                let Some(parent) = self.table(&key.table) else {
                    return invalid(format!(
                        "{} references unknown table {}",
                        table.sql_name, key.table
                    ));
                };
                let identity = parent.identity_columns();
                if key.references.len() != identity.len()
                    || key
                        .references
                        .iter()
                        .zip(&identity)
                        .any(|(name, column)| !name.eq_ignore_ascii_case(column))
                {
                    return invalid(format!(
                        "{}({}) must reference the identity of {}",
                        table.sql_name,
                        key.columns.join(","),
                        parent.sql_name
                    ));
                }
            }
            if let Some(like) = &table.granted_like
                && (!table.class.typed()
                    || like == &table.sql_name
                    || self.table(like).is_none_or(|parent| {
                        !parent.class.typed() || parent.granted_like.is_some()
                    }))
            {
                return invalid(format!("{}: invalid granted_like {like}", table.sql_name));
            }
            let mut partitions = BTreeSet::new();
            if table.partition_by.len() > 16
                || table.partition_by.iter().any(|name| {
                    !partitions.insert(name.to_ascii_lowercase())
                        || !table
                            .primary_key
                            .iter()
                            .any(|column| column.eq_ignore_ascii_case(name))
                        || table.column(name).is_none_or(|column| {
                            !column.not_null
                                || column.class != ColumnClass::Replicated
                                || column.affinity == Affinity::Numeric
                        })
                })
            {
                return invalid(format!(
                    "{}: partition columns must be distinct non-null primary-key columns",
                    table.sql_name
                ));
            }
            {
                let name = "id";
                let column = table.column(name).ok_or_else(|| {
                    SchemaError::Invalid(format!(
                        "{}.{} is required by descriptor format 7",
                        table.sql_name, name
                    ))
                })?;
                if column.sql_name != name
                    || column.affinity != Affinity::Blob
                    || !column.not_null
                    || column.collation != Collation::Binary
                    || column.class != ColumnClass::Replicated
                {
                    return invalid(format!(
                        "{}.{} must be a non-null BLOB identity column",
                        table.sql_name, name
                    ));
                }
                if !(table.primary_key.len() == table.identity_columns().len()
                    && table
                        .primary_key
                        .iter()
                        .zip(table.identity_columns())
                        .all(|(name, identity)| name.eq_ignore_ascii_case(&identity)))
                    && !table
                        .indexes
                        .iter()
                        .any(|index| table.is_identity_index(index))
                {
                    return invalid(format!(
                        "{} needs its unique identity index",
                        table.sql_name
                    ));
                }
            }
            let objects = [&table.sql_name]
                .into_iter()
                .chain(table.indexes.iter().map(|index| &index.name));
            for name in objects {
                if !names.insert(name.to_ascii_lowercase()) {
                    return invalid(format!("duplicate name {name}"));
                }
            }
        }
        for view in &self.views {
            let tokens = tokenize(&view.create_sql).unwrap_or_default();
            if !matches!(tokens.as_slice(), [Token::Word(create), Token::Word(kind), Token::Word(name) | Token::Quoted(name), Token::Word(as_word), ..] if create.eq_ignore_ascii_case("CREATE") && kind.eq_ignore_ascii_case("VIEW") && name == &view.name && as_word.eq_ignore_ascii_case("AS"))
                || tokens.contains(&Token::Symbol(';'))
            {
                return invalid(format!(
                    "view {} must contain one CREATE VIEW AS statement",
                    view.name
                ));
            }
            if !identifier(&view.name)
                || reserved(&view.name)
                || !names.insert(view.name.to_ascii_lowercase())
            {
                return invalid(format!("view name {}", view.name));
            }
        }
        let mut assertion_names = BTreeSet::new();
        for assertion in &self.assertions {
            let tokens = tokenize(&assertion.select_sql).unwrap_or_default();
            if !identifier(&assertion.name)
                || !assertion_names.insert(assertion.name.to_ascii_lowercase())
                || !matches!(tokens.first(), Some(Token::Word(word)) if word.eq_ignore_ascii_case("SELECT"))
                || tokens.contains(&Token::Symbol(';'))
            {
                return invalid(format!("assertion {} must be one SELECT", assertion.name));
            }
        }
        let mut restriction_names = BTreeSet::new();
        for restriction in &self.restrictions {
            let name = &restriction.name;
            if !identifier(name) || !restriction_names.insert(name.to_ascii_lowercase()) {
                return invalid(format!("restriction name {name}"));
            }
            let Some(table) = self.table(&restriction.table) else {
                return invalid(format!(
                    "restriction {name}: {} is not a table",
                    restriction.table
                ));
            };
            if !restriction.refuse_update && !restriction.refuse_delete {
                return invalid(format!("restriction {name} refuses nothing"));
            }
            if restriction.bind_when(table, |_| String::new()).is_none() {
                return invalid(format!(
                    "restriction {name}: when must be one rule expression over {}'s replicated columns",
                    table.sql_name
                ));
            }
        }
        Ok(())
    }
    pub fn restrictions_of<'a>(
        &'a self,
        table: &'a LogicalTable,
    ) -> impl Iterator<Item = &'a LogicalRestriction> {
        self.restrictions
            .iter()
            .filter(|restriction| restriction.table.eq_ignore_ascii_case(&table.sql_name))
    }
    pub fn check_stored_row_identity(bytes: &[u8]) -> Result<(), SchemaError> {
        #[derive(Deserialize)]
        struct Format {
            format: u32,
        }
        let format: Format = serde_json::from_slice(bytes)
            .map_err(|error| SchemaError::Invalid(error.to_string()))?;
        if format.format != LOGICAL_SCHEMA_FORMAT {
            return invalid("unsupported schema descriptor format");
        }
        Ok(())
    }
    pub fn require_row_identity(&self) -> Result<(), SchemaError> {
        if self.format != LOGICAL_SCHEMA_FORMAT {
            return invalid("unsupported schema descriptor format");
        }
        Ok(())
    }
    fn cross_class_reference(&self, table: &LogicalTable) -> Option<String> {
        table.foreign_keys.iter().find_map(|key| {
            self.tables
                .iter()
                .find(|target| target.sql_name.eq_ignore_ascii_case(&key.table))
                .filter(|target| target.class != table.class)
                .map(|_| key.columns.first().cloned().unwrap_or_default())
        })
    }
    pub fn installable(&self) -> Result<(), SchemaError> {
        self.require_row_identity()?;
        self.validate()?;
        let incompatible = |table: &str, reason| {
            Err(SchemaError::Incompatible {
                table: table.to_string(),
                reason,
            })
        };
        for table in &self.tables {
            if let Some(column) = self.cross_class_reference(table) {
                return incompatible(&table.sql_name, Incompatibility::ClassReference(column));
            }
            if let Err(reason) = table.installable() {
                return incompatible(&table.sql_name, reason);
            }
            for key in &table.foreign_keys {
                let Some(target) = self.table(&key.table) else {
                    return invalid(format!(
                        "{} references the undeclared table {}",
                        table.sql_name, key.table
                    ));
                };
                for (column, name) in key.columns.iter().zip(&key.references) {
                    let references_portable_identity = name.eq_ignore_ascii_case("id");
                    if references_portable_identity
                        && table
                            .column(column)
                            .is_some_and(|column| column.affinity != Affinity::Blob)
                    {
                        return invalid(format!(
                            "{}.{column} references {}.id and must be a blob",
                            table.sql_name, target.sql_name
                        ));
                    }
                }
                let reason = if !target.partition_by.is_empty()
                    && (table.partition_by.is_empty()
                        || table.partition_group.is_none()
                        || table.partition_group != target.partition_group)
                {
                    Some("partitioned references require the same explicit partition group")
                } else if !table.partition_by.is_empty() && target.partition_by.is_empty() {
                    Some("a partitioned child cannot reference an unpartitioned parent")
                } else {
                    None
                };
                if let Some(reason) = reason {
                    return incompatible(
                        &table.sql_name,
                        Incompatibility::CrossScopeReference(reason.into()),
                    );
                }
            }
        }
        for assertion in &self.assertions {
            if let Err(reason) = self.assertion_installable(assertion) {
                return incompatible(&assertion.name, reason);
            }
        }
        for restriction in &self.restrictions {
            if self
                .table(&restriction.table)
                .is_some_and(|table| table.class != TableClass::Replayed)
            {
                return incompatible(
                    &restriction.name,
                    Incompatibility::Restriction(format!(
                        "{} is not a replayed table",
                        restriction.table
                    )),
                );
            }
        }
        Ok(())
    }
    fn assertion_installable(&self, assertion: &LogicalAssertion) -> Result<(), Incompatibility> {
        use crate::deterministic::{AssertionShape, assertion_tables};
        let refused = |reason: &str| Err(Incompatibility::Assertion(reason.into()));
        let names = match assertion_tables(&assertion.select_sql) {
            AssertionShape::Reads(names) => names,
            AssertionShape::Function(function) => return Err(Incompatibility::Function(function)),
            AssertionShape::Refused(reason) => return refused(&reason),
        };
        let mut tables = Vec::with_capacity(names.len());
        for name in &names {
            match self.table(name) {
                Some(table) if table.class == TableClass::Replayed => tables.push(table),
                Some(_) => return refused(&format!("{name} is not a replayed table")),
                None => return refused(&format!("{name} is not a table")),
            }
        }
        fn group(table: &LogicalTable) -> Option<&str> {
            (!table.partition_by.is_empty())
                .then(|| table.partition_group.as_deref().unwrap_or(&table.sql_name))
        }
        let Some(first) = tables.first() else {
            return refused("reads no table");
        };
        if tables.iter().any(|table| group(table) != group(first)) {
            return refused("tables must share one partition group");
        }
        Ok(())
    }
    pub fn diagnose(&self, profile: Profile) -> Vec<SchemaError> {
        if let Err(error) = self.installable() {
            return vec![error];
        }
        if profile.replays_intents() {
            return Vec::new();
        }
        self.typed()
            .map(|table| SchemaError::Incompatible {
                table: table.sql_name.clone(),
                reason: Incompatibility::Unsupported(TableClass::Replayed),
            })
            .collect()
    }
    pub fn supported_on(&self, profile: Profile) -> Result<(), SchemaError> {
        match self.diagnose(profile).into_iter().next() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
    pub fn canonical(&self) -> Result<Vec<u8>, SchemaError> {
        self.validate()?;
        serde_json::to_vec(self).map_err(|error| SchemaError::Invalid(error.to_string()))
    }
    pub fn parse(bytes: &[u8]) -> Result<Self, SchemaError> {
        if bytes.len() > MAX_LOGICAL_SCHEMA_BYTES {
            return invalid("descriptor quota");
        }
        Self::check_stored_row_identity(bytes)?;
        let schema: Self = serde_json::from_slice(bytes)
            .map_err(|error| SchemaError::Invalid(error.to_string()))?;
        if schema.canonical()? != bytes {
            return invalid("descriptor is not canonical");
        }
        Ok(schema)
    }
    pub fn fingerprint(&self) -> Result<Commitment, SchemaError> {
        Ok(commitment("rocky.logical-schema.v1", &self.canonical()?))
    }
    pub fn replicated_fingerprint(&self) -> Commitment {
        let replicated = Replicated {
            format: self.format,
            tables: self
                .typed()
                .map(|table| replicated_table(self, table))
                .collect(),
        };
        commitment(
            "rocky.logical-schema.replicated.v1",
            json(&replicated).as_bytes(),
        )
    }
    pub fn typed(&self) -> impl Iterator<Item = &LogicalTable> {
        self.tables.iter().filter(|table| table.class.typed())
    }
    pub fn table(&self, sql_name: &str) -> Option<&LogicalTable> {
        self.tables
            .iter()
            .find(|table| table.sql_name.eq_ignore_ascii_case(sql_name))
    }
    pub fn table_id(&self, table: &LogicalTable) -> Commitment {
        definition_item_id(self.publisher, self.app_id(), "entity", &table.sql_name)
    }
    pub fn field_id(&self, table: &LogicalTable, column: &LogicalColumn) -> Commitment {
        let name = format!("{}.{}", table.sql_name, column.sql_name);
        definition_item_id(self.publisher, self.app_id(), "field", &name)
    }
    pub fn manifest(&self, domains: &[DomainId]) -> ApplicationManifest {
        self.manifest_at(domains, 1, crate::EngineIdentity::current())
    }
    pub fn manifest_at(
        &self,
        domains: &[DomainId],
        schema: u32,
        engine: crate::EngineIdentity,
    ) -> ApplicationManifest {
        let (publisher, app) = (self.publisher, self.app_id());
        let item = |kind: &str, name: String, shape: String| DefinitionItem {
            id: definition_item_id(publisher, app, kind, &name),
            name,
            shape,
        };
        let (mut entities, mut fields) = (Vec::new(), Vec::new());
        for table in self.typed() {
            let replicated = replicated_table(self, table);
            for column in replicated.key.iter().chain(&replicated.fields) {
                let name = format!("{}.{}", table.sql_name, column.name);
                fields.push(item("field", name, json(column)));
            }
            entities.push(item("entity", table.sql_name.clone(), json(&replicated)));
        }
        let shape = TYPED_RULE.to_string();
        ApplicationManifest {
            publisher,
            name: self.application.clone(),
            app,
            schema,
            projection: 1,
            engine,
            entities,
            fields,
            operations: ["insert", "update", "delete"]
                .into_iter()
                .map(|name| item("operation", name.to_string(), shape.clone()))
                .collect(),
            domains: domains
                .iter()
                .map(|domain| DomainRule {
                    domain: *domain,
                    author_log: AuthorLogScope::Domain,
                    rule: TYPED_RULE.into(),
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column(name: &str, affinity: Affinity) -> LogicalColumn {
        LogicalColumn {
            sql_name: name.into(),
            affinity,
            not_null: true,
            default: None,
            collation: Collation::Binary,
            class: ColumnClass::Replicated,
        }
    }
    fn schema() -> LogicalSchema {
        LogicalSchema {
            format: LOGICAL_SCHEMA_FORMAT,
            publisher: [7; 32],
            application: "shop".into(),
            source: SchemaSource {
                kind: "room".into(),
                version: 2,
                identity: "abc".into(),
            },
            tables: vec![LogicalTable {
                sql_name: "items".into(),
            granted_like: None,
                class: TableClass::Replayed,
                create_sql: "CREATE TABLE `items` (`key` TEXT NOT NULL, `title` TEXT NOT NULL, `id` BLOB NOT NULL, PRIMARY KEY(`key`))".into(),
                auto_generate: false,
                columns: vec![column("key", Affinity::Text), column("title", Affinity::Text), column("id", Affinity::Blob)],
                primary_key: vec!["key".into()],
                partition_by: Vec::new(),
            partition_group: None,
                indexes: vec![LogicalIndex { name: "items_identity".into(), unique: true, columns: vec![IndexColumn { name: "id".into(), descending: false }], create_sql: "CREATE UNIQUE INDEX items_identity ON items(id)".into(), expressions: Vec::new(), predicate: None }],
                foreign_keys: Vec::new(),

            }],
            views: Vec::new(),
            assertions: Vec::new(),
            restrictions: Vec::new(),
        }
    }
    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn a_foreign_key_references_exactly_the_identity_of_a_known_parent() {
        let refused = |table: &str, references: &[&str]| {
            let mut schema = schema();
            let items = &mut schema.tables[0];
            items.columns.push(column("link", Affinity::Blob));
            items.foreign_keys.push(LogicalForeignKey {
                columns: ["link", "title"][..references.len()]
                    .iter()
                    .map(|name| (*name).into())
                    .collect(),
                table: table.into(),
                references: references.iter().map(|name| (*name).into()).collect(),
                on_delete: DELETE_CASCADE.into(),
                on_update: "NO ACTION".into(),
                deferred: false,
            });
            schema.validate().err()
        };
        assert_eq!(refused("ITEMS", &["ID"]), None);
        let not_identity = Some(SchemaError::Invalid(
            "items(link) must reference the identity of items".into(),
        ));
        assert_eq!(refused("items", &["title"]), not_identity);
        assert_eq!(refused("items", &["key"]), not_identity);
        assert_eq!(
            refused("items", &["id", "title"]),
            Some(SchemaError::Invalid(
                "items(link,title) must reference the identity of items".into(),
            ))
        );
        assert_eq!(
            refused("missing", &["id"]),
            Some(SchemaError::Invalid(
                "items references unknown table missing".into()
            ))
        );
    }
    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn an_identity_index_covers_every_row_by_its_identity_columns_alone() {
        let identity = schema().tables[0].indexes[0].clone();
        let table = &schema().tables[0];
        assert!(table.is_identity_index(&identity));
        let partial = LogicalIndex {
            predicate: Some("title IS NOT NULL".into()),
            ..identity.clone()
        };
        let widened = LogicalIndex {
            expressions: vec!["lower(title)".into()],
            ..identity
        };
        assert!(!table.is_identity_index(&partial));
        assert!(!table.is_identity_index(&widened));
    }
    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn sql_column_and_reference_names_use_ascii_case_insensitive_lookup() {
        let mut schema = schema();
        let table = &mut schema.tables[0];
        table.primary_key = vec!["KEY".into()];
        assert_eq!(table.column("TITLE").unwrap().sql_name, "title");
        assert_eq!(table.key().next().unwrap().sql_name, "key");
        assert!(!table.fields().any(|column| column.sql_name == "key"));
        let key = LogicalForeignKey {
            columns: vec!["link".into()],
            table: "ITEMS".into(),
            references: vec!["ID".into()],
            on_delete: DELETE_CASCADE.into(),
            on_update: "NO ACTION".into(),
            deferred: false,
        };
        table.columns.push(column("link", Affinity::Blob));
        table.create_sql.pop();
        table
            .create_sql
            .push_str(&format!(",{})", foreign_key_clause(&key)));
        table.foreign_keys.push(key);
        schema.installable().unwrap();
        let mut duplicate = schema.clone();
        duplicate.tables[0]
            .columns
            .push(column("TITLE", Affinity::Text));
        assert_eq!(
            duplicate.validate(),
            Err(SchemaError::Invalid("items column TITLE".into()))
        );
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn local_sql_cannot_hide_an_undeclared_reference() {
        let mut schema = schema();
        schema.tables[0].class = TableClass::LocalOnly;
        schema.tables[0].create_sql = "CREATE TABLE items (id INTEGER REFERENCES items(id))".into();
        assert_eq!(
            schema.installable(),
            Err(SchemaError::Incompatible {
                table: "items".into(),
                reason: Incompatibility::ForeignKey,
            })
        );
        schema.tables[0].create_sql =
            "CREATE TABLE items (id INTEGER, FOREIGN KEY(id) REFERENCES items(id))".into();
        assert!(matches!(
            schema.installable(),
            Err(SchemaError::Incompatible {
                reason: Incompatibility::ForeignKey,
                ..
            })
        ));
        schema.tables[0].create_sql =
            "CREATE TABLE items (id INTEGER, title TEXT DEFAULT 'REFERENCES')".into();
        schema.installable().unwrap();
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn canonical_descriptors_round_trip_and_reject_other_bytes() {
        let schema = schema();
        let bytes = schema.canonical().unwrap();
        assert_eq!(LogicalSchema::parse(&bytes).unwrap(), schema);
        let spaced = String::from_utf8(bytes.clone()).unwrap().replace(':', ": ");
        assert!(LogicalSchema::parse(spaced.as_bytes()).is_err());
        let unknown = String::from_utf8(bytes)
            .unwrap()
            .replacen('{', "{\"x\":1,", 1);
        assert!(LogicalSchema::parse(unknown.as_bytes()).is_err());
        let mut reserved = schema.clone();
        reserved.tables[0].sql_name = "rocky_items".into();
        assert!(reserved.validate().is_err());
        let mut missing = schema;
        missing.tables[0].primary_key = vec!["absent".into()];
        assert!(missing.validate().is_err());
    }
    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn local_details_keep_the_replicated_identity_and_renames_do_not() {
        let schema = schema();
        let mut local = schema.clone();
        local.source.identity = "def".into();
        local.installable().unwrap();
        assert_ne!(schema.fingerprint().unwrap(), local.fingerprint().unwrap());
        assert_eq!(
            schema.replicated_fingerprint(),
            local.replicated_fingerprint()
        );
        let mut indexed = schema.clone();
        indexed.tables[0].indexes.push(LogicalIndex {
            name: "by_title".into(),
            unique: false,
            columns: vec![IndexColumn {
                name: "title".into(),
                descending: false,
            }],
            create_sql: "CREATE INDEX by_title ON items(title)".into(),
            expressions: Vec::new(),
            predicate: None,
        });
        assert_ne!(
            schema.replicated_fingerprint(),
            indexed.replicated_fingerprint(),
            "replay enforces indexes, so replicas agree on them"
        );
        let domain = [DomainId([1; 16])];
        assert_eq!(
            schema.manifest(&domain).hash(),
            local.manifest(&domain).hash()
        );
        schema.manifest(&domain).validate().unwrap();
        let mut renamed = schema.clone();
        let table = &mut renamed.tables[0];
        table.sql_name = "shopping".into();
        table.columns[1].sql_name = "name".into();
        table.create_sql = table
            .create_sql
            .replace("items", "shopping")
            .replace("title", "name");
        renamed.installable().unwrap();
        assert_ne!(
            schema.manifest(&domain).hash(),
            renamed.manifest(&domain).hash(),
            "names are identities: a renamed table or column is another item"
        );
        assert_ne!(
            schema.field_id(&schema.tables[0], &schema.tables[0].columns[1]),
            renamed.field_id(&renamed.tables[0], &renamed.tables[0].columns[1])
        );
        let mut retyped = schema.clone();
        retyped.tables[0].columns[1].affinity = Affinity::Blob;
        assert_ne!(
            schema.replicated_fingerprint(),
            retyped.replicated_fingerprint(),
            "a new type is another definition"
        );
        let mut published = schema.clone();
        published.publisher = [8; 32];
        assert_ne!(
            schema.app_id(),
            published.app_id(),
            "another publisher is another definition"
        );
        assert_ne!(
            schema.table_id(&schema.tables[0]),
            published.table_id(&published.tables[0])
        );
    }
    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn replayed_tables_refuse_mixed_references() {
        let reason = |child: TableClass, parent: TableClass| {
            let mut schema = schema();
            let mut parents = schema.tables[0].clone();
            parents.sql_name = "parents".into();
            parents.create_sql = parents.create_sql.replace("`items`", "`parents`");
            for index in &mut parents.indexes {
                index.name = index.name.replace("items", "parents");
            }
            parents.class = parent;
            let items = &mut schema.tables[0];
            items.class = child;
            let key = LogicalForeignKey {
                table: "parents".into(),
                columns: vec!["id".into()],
                references: vec!["id".into()],
                on_delete: DELETE_CASCADE.into(),
                on_update: "NO ACTION".into(),
                deferred: false,
            };
            items.create_sql.pop();
            items.create_sql += &format!(", {})", foreign_key_clause(&key));
            items.foreign_keys.push(key);
            schema.tables.push(parents);
            schema.tables.sort_by(|a, b| a.sql_name.cmp(&b.sql_name));
            match schema.installable() {
                Err(SchemaError::Incompatible { reason, .. }) => Some(reason),
                Ok(()) => None,
                Err(error) => panic!("{error}"),
            }
        };
        use TableClass::{LocalOnly, Replayed};
        let mixed = Some(Incompatibility::ClassReference("id".into()));
        assert_eq!(reason(Replayed, LocalOnly), mixed);
        assert_eq!(reason(LocalOnly, Replayed), mixed);
        assert_eq!(reason(Replayed, Replayed), None);
        let schema = schema();
        assert_eq!(schema.installable(), Ok(()));
        assert_eq!(
            schema.supported_on(Profile::Trusted),
            Err(SchemaError::Incompatible {
                table: "items".into(),
                reason: Incompatibility::Unsupported(Replayed),
            }),
            "only the secure profile replays"
        );
        assert_eq!(schema.supported_on(Profile::Secure), Ok(()));
    }
    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn references_must_be_declared_and_replayable() {
        let reason = |change: &dyn Fn(&mut LogicalTable)| {
            let mut schema = schema();
            change(&mut schema.tables[0]);
            match schema.installable() {
                Err(SchemaError::Incompatible { reason, .. }) => Some(reason),
                Ok(()) => None,
                Err(error) => panic!("{error}"),
            }
        };
        assert_eq!(reason(&|_| {}), None);
        assert_eq!(
            reason(&|table| table.foreign_keys.push(LogicalForeignKey {
                table: "items".into(),
                columns: vec!["title".into()],
                references: vec!["id".into()],
                on_delete: "NO ACTION".into(),
                on_update: "NO ACTION".into(),
                deferred: false,
            })),
            Some(Incompatibility::ForeignKey)
        );
        assert_eq!(
            reason(&|table| table.create_sql.push_str(" CHECK(random() > 0)")),
            Some(Incompatibility::Function("random".into()))
        );
        assert_eq!(
            reason(&|table| {
                table.class = TableClass::LocalOnly;
                table.auto_generate = true;
            }),
            None,
            "local constraints are SQLite's own on a local-only table"
        );
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn derived_rules_are_single_deterministic_expressions() {
        let columns = ["title", "count"];
        for rule in [
            "lower(title)",
            "trim(title) || '-' || count",
            "CASE WHEN count > 0 THEN upper(\"title\") ELSE '' END",
        ] {
            assert!(derived_rule(rule, &columns), "{rule}");
        }
        for rule in [
            "",
            "random()",
            "other",
            "(SELECT 1)",
            "lower(title); DROP TABLE items",
            "items.title",
            "lower(?)",
            "lower('open",
            "datetime('now')",
        ] {
            assert!(!derived_rule(rule, &columns), "{rule}");
        }
    }
    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn rules_bind_columns_and_statements_declare_collations() {
        let bind = |column: &str| (column == "title").then(|| "?2".to_string());
        assert_eq!(
            bind_rule("lower( title ) || 'title'", bind).as_deref(),
            Some("lower( ?2 ) || 'title'")
        );
        assert_eq!(bind_rule("lower(other)", bind), None);
        assert_eq!(
            declared_collations(
                "CREATE TABLE t(`a` TEXT COLLATE NOCASE NOT NULL, b TEXT, c TEXT COLLATE rtrim, PRIMARY KEY(a, b))"
            ),
            vec![
                ("a".to_string(), "NOCASE".to_string()),
                ("c".to_string(), "rtrim".to_string())
            ]
        );
    }
    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn affinity_follows_the_declared_type() {
        for (declared, affinity) in [
            ("INTEGER", Affinity::Integer),
            ("VARCHAR(10)", Affinity::Text),
            ("", Affinity::Blob),
            ("DOUBLE", Affinity::Real),
            ("DECIMAL", Affinity::Numeric),
        ] {
            assert_eq!(Affinity::of_declared(declared), affinity);
        }
    }
}
