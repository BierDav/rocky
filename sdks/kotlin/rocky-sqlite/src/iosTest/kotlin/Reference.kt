package rocky.sqlite

// iOS SQLite's default real rendering has more digits; Rocky retains its 15-digit contract.
internal actual fun referenceSql(textConversion: Boolean): String = if (textConversion) {
    "WITH value(x) AS (SELECT ?) SELECT CASE WHEN typeof(x) = 'real' THEN printf('%!.15g', x) ELSE x END FROM value"
} else {
    "SELECT ?"
}
