/* contrib/remotexact/remotexact.c */
#include "postgres.h"

#include "access/xact.h"
#include "access/remotexact.h"
#include "fmgr.h"
#include "libpq-fe.h"
#include "libpq/pqformat.h"
#include "utils/guc.h"
#include "utils/memutils.h"
#include "utils/rel.h"
#include "miscadmin.h"

PG_MODULE_MAGIC;

void		_PG_init(void);

/* GUCs */
char	   *xactserver_connstring;

typedef struct
{
	MemoryContext memctx;
	StringInfoData buf;
} RWSetData;

typedef RWSetData *RWSet;

RWSet		CurrentReadWriteSet = NULL;
PGconn	   *XactServerConn;
bool		Connected = false;

static void
init_read_write_set(void)
{
	MemoryContext oldcontext;

	Assert(!CurrentReadWriteSet);
	Assert(MemoryContextIsValid(TopTransactionContext));

	oldcontext = MemoryContextSwitchTo(TopTransactionContext);

	CurrentReadWriteSet = (RWSet) palloc(sizeof(RWSetData));
	CurrentReadWriteSet->memctx = TopTransactionContext;
	initStringInfo(&(CurrentReadWriteSet->buf));

	MemoryContextSwitchTo(oldcontext);
}

static void
rx_collect_read_tid(Relation relation, ItemPointer tid, TransactionId tuple_xid)
{
	StringInfo	buf = NULL;

	/*
	 * Ignore current tuple if this relation is an index or current xact wrote
	 * it.
	 */
	if (relation->rd_index != NULL || TransactionIdIsCurrentTransactionId(tuple_xid))
		return;

	if (CurrentReadWriteSet == NULL)
		init_read_write_set();

	buf = &(CurrentReadWriteSet->buf);

	pq_sendint32(buf, relation->rd_node.dbNode);
	pq_sendint32(buf, relation->rd_id);
	pq_sendint32(buf, ItemPointerGetBlockNumber(tid));
	pq_sendint16(buf, ItemPointerGetOffsetNumber(tid));
}

static void
rx_collect_seq_scan_rel_id(Relation relation)
{
	StringInfo	buf = NULL;

	if (CurrentReadWriteSet == NULL)
		init_read_write_set();

	buf = &(CurrentReadWriteSet->buf);

	pq_sendint32(buf, relation->rd_node.dbNode);
	pq_sendint32(buf, relation->rd_id);
	pq_sendint32(buf, -1);
	pq_sendint16(buf, -1);
}

static void
rx_collect_index_scan_page_id(Relation relation, BlockNumber blkno)
{
	StringInfo	buf = NULL;

	if (CurrentReadWriteSet == NULL)
		init_read_write_set();

	buf = &(CurrentReadWriteSet->buf);

	pq_sendint32(buf, relation->rd_node.dbNode);
	pq_sendint32(buf, relation->rd_id);
	pq_sendint32(buf, blkno);
	pq_sendint16(buf, -1);
}

static void
rx_clear_rwset(void)
{
	if (CurrentReadWriteSet == NULL)
		return;

	pfree(CurrentReadWriteSet->buf.data);
	pfree(CurrentReadWriteSet);
	CurrentReadWriteSet = NULL;
}

static bool
connect_to_txn_server(void)
{
	PGresult   *res;

	/* Reconnect if the connection is bad for some reason */
	if (Connected && PQstatus(XactServerConn) == CONNECTION_BAD)
	{
		PQfinish(XactServerConn);
		XactServerConn = NULL;
		Connected = false;

		ereport(LOG, errmsg("[remotexact] connection to transaction server broken, reconnecting..."));
	}

	if (Connected)
	{
		ereport(LOG, errmsg("[remotexact] reuse existing connection to transaction server"));
		return true;
	}

	XactServerConn = PQconnectdb(xactserver_connstring);

	if (PQstatus(XactServerConn) == CONNECTION_BAD)
	{
		char	   *msg = pchomp(PQerrorMessage(XactServerConn));

		PQfinish(XactServerConn);
		ereport(WARNING,
				errmsg("[remotexact] could not connect to the transaction server"),
				errdetail_internal("%s", msg));
		return Connected;
	}

	res = PQexec(XactServerConn, "start");
	if (PQresultStatus(res) != PGRES_COPY_BOTH)
	{
		ereport(WARNING, errmsg("[remotexact] invalid response from transaction server"));
		return Connected;
	}
	PQclear(res);

	Connected = true;

	ereport(LOG, errmsg("[remotexact] connected to transaction server"));

	return Connected;
}

static void
rx_send_rwset_and_wait(void)
{
	StringInfo	buf;

	if (CurrentReadWriteSet == NULL)
		return;

	if (!connect_to_txn_server())
		return;

	buf = &CurrentReadWriteSet->buf;

	if (PQputCopyData(XactServerConn, buf->data, buf->len) <= 0 || PQflush(XactServerConn))
	{
		ereport(WARNING, errmsg("[remotexact] failed to send read/write set"));
	}
}

static const RemoteXactHook remote_xact_hook =
{
	.collect_read_tid = rx_collect_read_tid,
	.collect_seq_scan_rel_id = rx_collect_seq_scan_rel_id,
	.collect_index_scan_page_id = rx_collect_index_scan_page_id,
	.clear_rwset = rx_clear_rwset,
	.send_rwset_and_wait = rx_send_rwset_and_wait
};

void
_PG_init(void)
{
	if (!process_shared_preload_libraries_in_progress)
 		return;

	DefineCustomStringVariable("remotexact.connstring",
							   "connection string to the transaction server",
							   NULL,
							   &xactserver_connstring,
							   "postgresql://127.0.0.1:10000",
							   PGC_POSTMASTER,
							   0,	/* no flags required */
							   NULL, NULL, NULL);

	if (xactserver_connstring && xactserver_connstring[0]) {
		SetRemoteXactHook(&remote_xact_hook);

		ereport(LOG, errmsg("[remotexact] initialized"));
		ereport(LOG, errmsg("[remotexact] xactserver connection string \"%s\"", xactserver_connstring));
	}
}
