#include "postgres.h"

#include <fcntl.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>

#include "access/rwset.h"
#include "access/xact.h"
#include "lib/stringinfo.h"
#include "utils/memutils.h"
#include "utils/rel.h"

typedef struct {
  MemoryContext memctx;
  StringInfoData buf;
} RWSetData;

typedef RWSetData* RWSet;

RWSet   CurrentReadWriteSet = NULL;

static void init_read_write_set(void);
static pgsocket connect_to_slog(void);
static bool send_buffer(pgsocket sock, char* buf, int len);

void CollectReadTID(Relation relation, ItemPointer tid, TransactionId tuple_xid) {
  StringInfo buf = NULL;
  Oid dbid;
  Oid relid;
  BlockNumber blockno;
  OffsetNumber offset;

	/*
	 * Return if this relation is an index or current xact wrote it.
	 */
  if (relation->rd_index != NULL || TransactionIdIsCurrentTransactionId(tuple_xid))
    return;


  if (CurrentReadWriteSet == NULL) {
    init_read_write_set();
  }

  buf = &(CurrentReadWriteSet->buf);
  dbid = relation->rd_node.dbNode;
  relid = relation->rd_id;
  blockno = ItemPointerGetBlockNumber(tid);
  offset = ItemPointerGetOffsetNumber(tid);

  appendBinaryStringInfo(buf, (char *) &dbid, sizeof(dbid));
  appendBinaryStringInfo(buf, (char *) &relid, sizeof(relid));
  appendBinaryStringInfo(buf, (char *) &blockno, sizeof(blockno));
  appendBinaryStringInfo(buf, (char *) &offset, sizeof(offset));
}

void SendRWSetAndWaitForCommit(void) {
  pgsocket sock;
  int len;
  int msglen;

  if (CurrentReadWriteSet == NULL) {
    return;
  }

  sock = connect_to_slog();
  if (sock == PGINVALID_SOCKET) {
    return;
  }

  len = CurrentReadWriteSet->buf.len;
  msglen = len + sizeof(len);
  if (!send_buffer(sock, (char *) &msglen, sizeof(msglen)))
    return;

  if (!send_buffer(sock, CurrentReadWriteSet->buf.data, len))
    return;

  close(sock);
}

void CleanUpRWSet(void) {
  if (CurrentReadWriteSet == NULL) {
    return;
  }

  pfree(CurrentReadWriteSet->buf.data);
  pfree(CurrentReadWriteSet);
  CurrentReadWriteSet = NULL;
}

static void init_read_write_set(void) {
	MemoryContext oldcontext;

  Assert(!CurrentReadWriteSet);
  Assert(MemoryContextIsValid(TopTransactionContext));

  oldcontext = MemoryContextSwitchTo(TopTransactionContext);

  CurrentReadWriteSet = (RWSet) palloc(sizeof(RWSetData));
  CurrentReadWriteSet->memctx = TopTransactionContext;
  initStringInfo(&(CurrentReadWriteSet->buf));

  MemoryContextSwitchTo(oldcontext);
}

static pgsocket connect_to_slog(void) {
  struct sockaddr_un addr;
  pgsocket sock;
  char* path = "/tmp/slogora";
  size_t pathlen = strlen(path);
  socklen_t addrlen;

  sock = socket(AF_UNIX, SOCK_STREAM, 0);
  if (sock == PGINVALID_SOCKET) {
    ereport(WARNING, errmsg("could not create a socket"));
    return PGINVALID_SOCKET;
  }
  
  addr.sun_family = AF_UNIX;
  memcpy(addr.sun_path, path, pathlen + 1);
  addrlen = (socklen_t)(offsetof(struct sockaddr_un, sun_path) + pathlen);

  if (connect(sock, (struct sockaddr_un*) &addr, addrlen) < 0) {
    ereport(WARNING, errmsg("could not connect to SLOG"));
    return PGINVALID_SOCKET;
  }

  return sock;
}

static bool send_buffer(pgsocket sock, char* buf, int len) {
  int total_sent = 0;
  while (total_sent < len) {
    int sent = send(sock, buf + total_sent, len - total_sent, 0);
    if (sent < 0) {
      return false;
    }
    total_sent += sent;
  }
  return true;
}