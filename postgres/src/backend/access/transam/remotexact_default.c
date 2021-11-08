/*-------------------------------------------------------------------------
 *
 * remotexact_default.c
 *
 * IDENTIFICATION
 *	  src/backend/access/transam/remotexact_default.c
 *
 *-------------------------------------------------------------------------
 */

#include "access/remotexact.h"

static void
default_collect_read_tid(Relation relation, ItemPointer tid, TransactionId tuple_xid)
{
}

static void
default_collect_seq_scan_rel_id(Relation relation)
{
}

static void
default_collect_index_scan_page_id(Relation relation, BlockNumber blkno)
{
}

static void
default_clear_rwset(void)
{
}

static void
default_send_rwset_and_wait(void)
{
}

static const RemoteXactHook default_hook = {
	.collect_read_tid = default_collect_read_tid,
	.collect_seq_scan_rel_id = default_collect_seq_scan_rel_id,
	.collect_index_scan_page_id = default_collect_index_scan_page_id,
	.clear_rwset = default_clear_rwset,
	.send_rwset_and_wait = default_send_rwset_and_wait
};

static RemoteXactHook *remote_xact_hook = &default_hook;

void
SetRemoteXactHook(const RemoteXactHook *hook)
{
	Assert(hook != NULL);
	remote_xact_hook = hook;
}

RemoteXactHook *
GetRemoteXactHook(void)
{
	return remote_xact_hook;
}
