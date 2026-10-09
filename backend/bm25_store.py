"""Exact, transactional BM25 postings. Only changed records update corpus statistics."""
from collections import Counter
from contextlib import contextmanager
import json
import math
import sqlite3

from backend.inference.lexical_tokens import tokenize_current
from backend.interface import EmbeddingContractError


@contextmanager
def connect(journal, *, create=False):
    path = (journal.directory / "bm25.sqlite").resolve()
    if not create and not path.is_file():
        raise EmbeddingContractError("INDEX_SPARSE_STALE")
    db = sqlite3.connect(path if create else path.as_uri() + "?mode=rw", uri=not create)
    try:
        db.execute("PRAGMA foreign_keys=ON")
        if create:
            db.execute("PRAGMA journal_mode=WAL")
            db.execute("PRAGMA synchronous=FULL")
            db.executescript("""
                CREATE TABLE IF NOT EXISTS docs (
                    id TEXT PRIMARY KEY, length INTEGER NOT NULL, dataset TEXT, file TEXT, role TEXT);
                CREATE INDEX IF NOT EXISTS docs_scope ON docs(dataset, file);
                CREATE TABLE IF NOT EXISTS postings (
                    term TEXT, doc TEXT REFERENCES docs(id) ON DELETE CASCADE,
                    tf INTEGER NOT NULL, PRIMARY KEY(term, doc)) WITHOUT ROWID;
                CREATE INDEX IF NOT EXISTS postings_doc ON postings(doc);
                CREATE TABLE IF NOT EXISTS frequencies (term TEXT PRIMARY KEY, df INTEGER NOT NULL);
                CREATE INDEX IF NOT EXISTS frequencies_df ON frequencies(df);
                CREATE TABLE IF NOT EXISTS ancestors (
                    ancestor TEXT, doc TEXT REFERENCES docs(id) ON DELETE CASCADE,
                    PRIMARY KEY(ancestor, doc)) WITHOUT ROWID;
                CREATE INDEX IF NOT EXISTS ancestors_doc ON ancestors(doc);
                CREATE TABLE IF NOT EXISTS corpus (
                    id INTEGER PRIMARY KEY CHECK(id=1), n INTEGER, length INTEGER, revision TEXT);
                INSERT OR IGNORE INTO corpus VALUES (1, 0, 0, '');
            """)
        yield db
    finally:
        db.close()


def metadata(journal):
    try:
        with connect(journal) as db:
            row = db.execute("SELECT n, length, revision FROM corpus WHERE id=1").fetchone()
            if not row:
                raise EmbeddingContractError("INDEX_SPARSE_CONTRACT_INVALID")
            return row
    except sqlite3.DatabaseError as error:
        raise EmbeddingContractError("INDEX_SPARSE_CONTRACT_INVALID") from error


def reset(db):
    db.execute("DELETE FROM docs")
    db.execute("DELETE FROM frequencies")
    db.execute("UPDATE corpus SET n=0, length=0, revision='' WHERE id=1")


def remove(db, point_id):
    row = db.execute("SELECT length FROM docs WHERE id=?", (point_id,)).fetchone()
    if row is None:
        return
    db.execute("UPDATE frequencies SET df=df-1 WHERE term IN (SELECT term FROM postings WHERE doc=?)", (point_id,))
    db.execute("DELETE FROM docs WHERE id=?", (point_id,))
    db.execute("UPDATE corpus SET n=n-1, length=length-? WHERE id=1", (row[0],))


def clear_scope(db, scope):
    if "ids" in scope:
        for point_id in scope["ids"]:
            remove(db, json.dumps(point_id))
        return
    where, args = (("dataset IN (SELECT value FROM json_each(?))", [json.dumps(scope["datasets"])])
                   if "datasets" in scope else ("dataset=?", [scope["dataset"]]))
    if "file" in scope:
        where += " AND file=?"
        args.append(scope["file"])
    # Deleting while iterating the same SQLite cursor is undefined: use bounded batches.
    while rows := db.execute(f"SELECT id FROM docs WHERE {where} LIMIT 128", args).fetchall():
        for (point_id,) in rows:
            remove(db, point_id)


def put(db, points):
    for point in points:
        point_id = json.dumps(point.id)
        remove(db, point_id)
        payload = point.payload or {}
        terms = Counter(tokenize_current(str(payload.get("text") or "")))
        length = sum(terms.values())
        db.execute("INSERT INTO docs VALUES (?, ?, ?, ?, ?)", (
            point_id, length, payload.get("dataset_id"), payload.get("file_name"), payload.get("node_role")))
        db.executemany("INSERT INTO postings VALUES (?, ?, ?)",
                       ((term, point_id, tf) for term, tf in terms.items()))
        db.executemany("INSERT INTO frequencies VALUES (?, 1) ON CONFLICT(term) DO UPDATE SET df=df+1",
                       ((term,) for term in terms))
        db.executemany("INSERT OR IGNORE INTO ancestors VALUES (?, ?)",
                       ((ancestor, point_id) for ancestor in payload.get("ancestor_ids") or []))
        db.execute("UPDATE corpus SET n=n+1, length=length+? WHERE id=1", (length,))


def search(journal, text, *, limit=24, dataset_ids=None, doc_filter=None,
           node_roles=None, ancestor_ids=None):
    """Typed Qdrant IDs and exact scores; filter BEFORE taking top-k."""
    stamp = journal.read_stamp()
    terms = sorted(set(tokenize_current(text)))
    if not terms:
        return []
    with connect(journal) as db:
        db.execute("BEGIN")
        n, length, revision = db.execute("SELECT n, length, revision FROM corpus WHERE id=1").fetchone()
        if revision != stamp:
            raise EmbeddingContractError("INDEX_SPARSE_STALE")
        if not n:
            return []
        db.execute("CREATE TEMP TABLE query_terms (term TEXT PRIMARY KEY, idf REAL)")
        for term in terms:
            row = db.execute("SELECT df FROM frequencies WHERE term=?", (term,)).fetchone()
            if row and row[0] > 0:
                db.execute("INSERT INTO query_terms VALUES (?, ?)",
                           (term, math.log1p((n - row[0] + .5) / (row[0] + .5))))
        clauses, args = [], [length / n if length else 1.]
        for column, values in (("dataset", dataset_ids), ("file", doc_filter), ("role", node_roles)):
            if values:
                clauses.append(f"d.{column} IN (SELECT value FROM json_each(?))")
                args.append(json.dumps(values))
        if ancestor_ids:
            clauses.append("EXISTS (SELECT 1 FROM ancestors a WHERE a.doc=d.id "
                           "AND a.ancestor IN (SELECT value FROM json_each(?)))")
            args.append(json.dumps(ancestor_ids))
        where = " AND ".join(clauses) or "1"
        rows = db.execute(f"""
            SELECT d.id, SUM(q.idf * p.tf * 2.2 / (p.tf + 1.2 * (.25 + .75 * d.length / ?))) AS score
            FROM query_terms q CROSS JOIN postings p ON p.term=q.term JOIN docs d ON d.id=p.doc
            WHERE {where} GROUP BY d.id ORDER BY score DESC, d.id LIMIT ?
        """, [*args, int(limit)]).fetchall()
    journal.assert_unchanged(stamp)
    return [(json.loads(point_id), score) for point_id, score in rows]
