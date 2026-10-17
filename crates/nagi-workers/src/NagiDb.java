// JDBC helper for Nagi. Run as: java -cp <driver jar> NagiDb.java ; talks line by line over stdin/stdout.
// ASCII only on purpose: the source launcher reads this file with the platform encoding (MS932 on Japanese Windows).
// Fields are tab separated; \ \t \n \r are escaped, NULL is \N. The last line of each reply starts with OK or ERR.
//   CONNECT url user pass  -> OK product schema
//   TABLES schema          -> T name type ... OK
//   COLUMNS schema table   -> C name type size nullable ... OK
//   QUERY max sql          -> H col... / R v... / OK rows n more ms   or   OK updated count ms
import java.io.*;
import java.nio.charset.StandardCharsets;
import java.sql.*;
import java.util.*;

public class NagiDb {
    static PrintStream out;
    static Connection conn;

    static String esc(String s) {
        if (s == null) return "\\N";
        StringBuilder b = new StringBuilder(s.length());
        for (char c : s.toCharArray()) {
            switch (c) {
                case '\\': b.append("\\\\"); break;
                case '\t': b.append("\\t"); break;
                case '\n': b.append("\\n"); break;
                case '\r': b.append("\\r"); break;
                default: b.append(c);
            }
        }
        return b.toString();
    }

    static String unesc(String s) {
        StringBuilder b = new StringBuilder(s.length());
        for (int i = 0; i < s.length(); i++) {
            char c = s.charAt(i);
            if (c == '\\' && i + 1 < s.length()) {
                char n = s.charAt(++i);
                b.append(n == 't' ? '\t' : n == 'n' ? '\n' : n == 'r' ? '\r' : n);
            } else b.append(c);
        }
        return b.toString();
    }

    static void line(String... f) {
        StringBuilder b = new StringBuilder();
        for (int i = 0; i < f.length; i++) {
            if (i > 0) b.append('\t');
            b.append(f[i]);
        }
        out.println(b);
    }

    static String schemaOr(String s) throws SQLException {
        if (!s.isEmpty()) return s;
        String cur = null;
        try { cur = conn.getSchema(); } catch (Throwable e) { }
        if (cur == null) cur = conn.getMetaData().getUserName();
        return cur;
    }

    static String cell(ResultSet rs, int i) {
        try { return rs.getString(i); } catch (Throwable e) { return "(" + e.getClass().getSimpleName() + ")"; }
    }

    static void handle(String[] a) throws Exception {
        switch (a[0]) {
            case "CONNECT": {
                if (conn != null) try { conn.close(); } catch (Throwable e) { }
                Properties p = new Properties();
                if (!a[2].isEmpty()) p.setProperty("user", a[2]);
                if (!a[3].isEmpty()) p.setProperty("password", a[3]);
                conn = DriverManager.getConnection(a[1], p);
                DatabaseMetaData md = conn.getMetaData();
                line("OK", esc(md.getDatabaseProductName() + " " + md.getDatabaseProductVersion().split("\n")[0]), esc(schemaOr("")));
                break;
            }
            case "TABLES": {
                String s = schemaOr(a[1]);
                try (ResultSet rs = conn.getMetaData().getTables(null, s, "%", new String[] {"TABLE", "VIEW"})) {
                    while (rs.next()) line("T", esc(rs.getString("TABLE_NAME")), esc(rs.getString("TABLE_TYPE")));
                }
                line("OK");
                break;
            }
            case "COLUMNS": {
                String s = schemaOr(a[1]);
                try (ResultSet rs = conn.getMetaData().getColumns(null, s, a[2], "%")) {
                    while (rs.next()) line("C", esc(rs.getString("COLUMN_NAME")), esc(rs.getString("TYPE_NAME")), esc(rs.getString("COLUMN_SIZE")), rs.getInt("NULLABLE") == DatabaseMetaData.columnNoNulls ? "0" : "1");
                }
                line("OK");
                break;
            }
            case "QUERY": {
                int max = Integer.parseInt(a[1]);
                String sql = a[2].trim();
                String up = sql.toUpperCase(Locale.ROOT);
                // drop a trailing ; except for PL/SQL blocks (Oracle JDBC rejects it)
                if (sql.endsWith(";") && !(up.startsWith("BEGIN") || up.startsWith("DECLARE") || up.contains("END;"))) sql = sql.substring(0, sql.length() - 1);
                long t0 = System.nanoTime();
                try (Statement st = conn.createStatement()) {
                    st.setMaxRows(max + 1);
                    boolean rsFirst = st.execute(sql);
                    if (rsFirst) {
                        try (ResultSet rs = st.getResultSet()) {
                            ResultSetMetaData m = rs.getMetaData();
                            int n = m.getColumnCount();
                            String[] h = new String[n + 1];
                            h[0] = "H";
                            for (int i = 1; i <= n; i++) h[i] = esc(m.getColumnLabel(i));
                            line(h);
                            int count = 0;
                            boolean more = false;
                            while (rs.next()) {
                                if (count == max) { more = true; break; }
                                String[] r = new String[n + 1];
                                r[0] = "R";
                                for (int i = 1; i <= n; i++) r[i] = esc(cell(rs, i));
                                line(r);
                                count++;
                            }
                            line("OK", "rows", String.valueOf(count), more ? "1" : "0", String.valueOf((System.nanoTime() - t0) / 1000000));
                        }
                    } else {
                        line("OK", "updated", String.valueOf(st.getUpdateCount()), String.valueOf((System.nanoTime() - t0) / 1000000));
                    }
                }
                break;
            }
            default:
                line("ERR", esc("unknown request: " + a[0]));
        }
    }

    public static void main(String[] args) throws Exception {
        out = new PrintStream(new FileOutputStream(FileDescriptor.out), true, "UTF-8");
        BufferedReader in = new BufferedReader(new InputStreamReader(System.in, StandardCharsets.UTF_8));
        line("READY");
        String l;
        while ((l = in.readLine()) != null) {
            String[] raw = l.split("\t", -1);
            String[] a = new String[Math.max(raw.length, 4)];
            Arrays.fill(a, "");
            for (int i = 0; i < raw.length; i++) a[i] = unesc(raw[i]);
            try {
                handle(a);
            } catch (Throwable e) {
                String m = e.getMessage() == null ? e.toString() : e.getMessage();
                line("ERR", esc(m.trim()));
            }
        }
        if (conn != null) try { conn.close(); } catch (Throwable e) { }
    }
}
