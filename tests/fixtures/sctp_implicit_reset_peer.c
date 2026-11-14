/* Independent usrsctp wire control for server issue #1184. */
#if defined(NDEBUG)
#error Assertions must remain enabled for this qualification probe
#endif
#include <arpa/inet.h>
#include <assert.h>
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <time.h>
#include <usrsctp.h>

struct link {
  int index;
  unsigned outgoing_one;
  unsigned messages;
};
struct packet {
  struct packet *next;
  struct link *source;
  size_t length;
  uint8_t bytes[];
};
static struct link links[2] = {{0, 0, 0}, {1, 0, 0}};
static struct packet *first, *last, *held;
static uint32_t reset_sequence, lost_tsn;
static int intercept, have_reset, have_lost, have_forward;
static unsigned old_zero, future_zero, future_one;
static uint16_t get16(const uint8_t *p) {
  return (uint16_t)((p[0] << 8) | p[1]);
}
static uint32_t get32(const uint8_t *p) {
  return ((uint32_t)get16(p) << 16) | get16(p + 2);
}
static void put32(uint8_t *p, uint32_t v) {
  p[0] = (uint8_t)(v >> 24);
  p[1] = (uint8_t)(v >> 16);
  p[2] = (uint8_t)(v >> 8);
  p[3] = (uint8_t)v;
}
static void checksum(struct packet *p) {
  memset(p->bytes + 8, 0, 4);
  uint32_t crc = usrsctp_crc32c(p->bytes, p->length);
  memcpy(p->bytes + 8, &crc, 4);
}
static struct packet *copy_packet(struct link *source, const void *bytes,
                                  size_t n) {
  assert(n >= 12 && n <= 65536);
  struct packet *p = malloc(sizeof(*p) + n);
  assert(p);
  p->next = NULL;
  p->source = source;
  p->length = n;
  memcpy(p->bytes, bytes, n);
  return p;
}
static int output(void *address, void *bytes, size_t n, uint8_t tos,
                  uint8_t df) {
  (void)tos;
  (void)df;
  struct packet *p = copy_packet(address, bytes, n);
  if (last)
    last->next = p;
  else
    first = p;
  last = p;
  return 0;
}
static int receive_message(struct socket *socket, union sctp_sockstore address,
                           void *data, size_t n, struct sctp_rcvinfo info,
                           int flags, void *context) {
  (void)socket;
  (void)address;
  struct link *link = context;
  if (!data)
    return 1;
  if (flags & MSG_NOTIFICATION) {
    union sctp_notification *notification = data;
    if (notification->sn_header.sn_type == SCTP_STREAM_RESET_EVENT) {
      struct sctp_stream_reset_event *r = &notification->sn_strreset_event;
      assert((r->strreset_flags &
              (SCTP_STREAM_RESET_DENIED | SCTP_STREAM_RESET_FAILED)) == 0);
      unsigned count = (unsigned)((n - sizeof(*r)) / sizeof(uint16_t));
      for (unsigned i = 0; i < count; i++)
        if (r->strreset_stream_list[i] == 1 &&
            (r->strreset_flags & SCTP_STREAM_RESET_OUTGOING_SSN))
          link->outgoing_one++;
    }
  } else {
    assert(link->index == 1 && info.rcv_sid == 1);
    if (link->messages == 0)
      assert(n == 3 && memcmp(data, "old", 3) == 0);
    else
      assert(n == 8 && memcmp(data, "reliable", 8) == 0 && info.rcv_ssn == 1);
    link->messages++;
  }
  free(data);
  return 1;
}
static void pump(void) {
  unsigned delivered = 0;
  while (first) {
    assert(++delivered < 10000);
    struct packet *p = first;
    first = p->next;
    if (!first)
      last = NULL;
    int drop = 0, changed = 0;
    for (size_t c = 12; c + 4 <= p->length;) {
      size_t length = get16(p->bytes + c + 2);
      assert(length >= 4 && c + length <= p->length);
      uint8_t type = p->bytes[c];
      if (type == 130) {
        for (size_t t = c + 4; t + 4 <= c + length;) {
          size_t size = get16(p->bytes + t + 2);
          assert(size >= 4 && t + size <= c + length);
          if (get16(p->bytes + t) == 13 && size == 18) {
            uint16_t stream = get16(p->bytes + t + 16);
            if (p->source->index == 0 && stream == 1) {
              reset_sequence = get32(p->bytes + t + 4);
              have_reset = 1;
            }
            if (intercept && p->source->index == 1 && stream == 2) {
              assert(have_reset && get32(p->bytes + t + 8) == reset_sequence &&
                     !held);
              held = copy_packet(p->source, p->bytes, p->length);
              drop = 1;
            }
          }
          if (intercept && p->source->index == 1 && get16(p->bytes + t) == 16 &&
              size >= 12 && have_reset &&
              get32(p->bytes + t + 4) == reset_sequence) {
            assert(get32(p->bytes + t + 8) == 1);
            put32(p->bytes + t + 8, 6);
            changed = 1;
          }
          t += (size + 3) & ~(size_t)3;
        }
      }
      if (type == 0 && p->source->index == 0 && get16(p->bytes + c + 8) == 1) {
        uint16_t sequence = get16(p->bytes + c + 10);
        if (length == 19 && memcmp(p->bytes + c + 16, "old", 3) == 0) {
          assert(sequence == 0);
          old_zero++;
        } else if (length == 20 && memcmp(p->bytes + c + 16, "lost", 4) == 0) {
          assert(sequence == 0);
          future_zero++;
          if (have_lost)
            assert(lost_tsn == get32(p->bytes + c + 4));
          lost_tsn = get32(p->bytes + c + 4);
          have_lost = 1;
          drop = 1;
        } else {
          assert(length == 24 && sequence == 1 &&
                 memcmp(p->bytes + c + 16, "reliable", 8) == 0);
          future_one++;
        }
      }
      if (type == 192 && p->source->index == 0) {
        assert(have_lost && get32(p->bytes + c + 4) == lost_tsn);
        assert(length == 12 && get16(p->bytes + c + 8) == 1 &&
               get16(p->bytes + c + 10) == 0);
        have_forward = 1;
      }
      c += (length + 3) & ~(size_t)3;
    }
    if (changed)
      checksum(p);
    if (!drop)
      usrsctp_conninput(&links[1 - p->source->index], p->bytes, p->length, 0);
    free(p);
  }
}
static struct socket *make_socket(struct link *link) {
  struct socket *s = usrsctp_socket(AF_CONN, SOCK_STREAM, IPPROTO_SCTP,
                                    receive_message, NULL, 0, link);
  assert(s);
  assert(usrsctp_set_non_blocking(s, 1) == 0);
  struct sctp_assoc_value reset = {SCTP_ALL_ASSOC,
                                   SCTP_ENABLE_RESET_STREAM_REQ};
  assert(usrsctp_setsockopt(s, IPPROTO_SCTP, SCTP_ENABLE_STREAM_RESET, &reset,
                            sizeof(reset)) == 0);
  struct sctp_event event = {SCTP_ALL_ASSOC, SCTP_STREAM_RESET_EVENT, 1};
  assert(usrsctp_setsockopt(s, IPPROTO_SCTP, SCTP_EVENT, &event,
                            sizeof(event)) == 0);
  struct sctp_rtoinfo retransmission_timeout = {0, 100, 100, 100};
  assert(usrsctp_setsockopt(s, IPPROTO_SCTP, SCTP_RTOINFO,
                            &retransmission_timeout,
                            sizeof(retransmission_timeout)) == 0);
  struct sockaddr_conn address = {0};
  address.sconn_family = AF_CONN;
  address.sconn_port = htons(5000);
  address.sconn_addr = link;
  assert(usrsctp_bind(s, (struct sockaddr *)&address, sizeof(address)) == 0);
  return s;
}
static void reset_stream(struct socket *s, uint16_t stream) {
  size_t length = sizeof(struct sctp_reset_streams) + sizeof(uint16_t);
  struct sctp_reset_streams *reset = calloc(1, length);
  assert(reset);
  reset->srs_flags = SCTP_STREAM_RESET_OUTGOING;
  reset->srs_number_streams = 1;
  reset->srs_stream_list[0] = stream;
  assert(usrsctp_setsockopt(s, IPPROTO_SCTP, SCTP_RESET_STREAMS, reset,
                            (socklen_t)length) == 0);
  free(reset);
  pump();
}
static void send_message(struct socket *s, const char *message, int timed) {
  struct sctp_sendv_spa info = {0};
  info.sendv_flags = SCTP_SEND_SNDINFO_VALID;
  info.sendv_sndinfo.snd_sid = 1;
  info.sendv_sndinfo.snd_ppid = htonl(53);
  if (timed) {
    info.sendv_flags |= SCTP_SEND_PRINFO_VALID;
    info.sendv_prinfo.pr_policy = SCTP_PR_SCTP_TTL;
    info.sendv_prinfo.pr_value = 1;
  }
  size_t n = strlen(message);
  assert(usrsctp_sendv(s, message, n, NULL, 0, &info, sizeof(info),
                       SCTP_SENDV_SPA, 0) == (ssize_t)n);
  pump();
}
int main(int argc, char **argv) {
  assert(argc == 1 || (argc == 2 && strcmp(argv[1], "--mismatch-first") == 0));
  int mismatch_first = argc == 2;
  usrsctp_init_nothreads(0, output, NULL);
  usrsctp_sysctl_set_sctp_ecn_enable(0);
  for (int i = 0; i < 2; i++)
    usrsctp_register_address(&links[i]);
  struct socket *sender = make_socket(&links[0]),
                *listener = make_socket(&links[1]);
  assert(usrsctp_listen(listener, 1) == 0);
  struct sockaddr_conn remote = {0};
  remote.sconn_family = AF_CONN;
  remote.sconn_port = htons(5000);
  remote.sconn_addr = &links[0];
  assert(usrsctp_connect(sender, (struct sockaddr *)&remote, sizeof(remote)) ==
             0 ||
         errno == EINPROGRESS);
  pump();
  struct socket *receiver = usrsctp_accept(listener, NULL, NULL);
  assert(receiver);
  usrsctp_close(listener);
  pump();
  send_message(sender, "old", 0);
  assert(old_zero == 1 && links[1].messages == 1);
  reset_stream(receiver, 1);
  assert(links[1].outgoing_one == 1);
  intercept = 1;
  reset_stream(sender, 1);
  assert(have_reset && links[0].outgoing_one == 0);
  reset_stream(receiver, 2);
  assert(held && links[0].outgoing_one == 0);
  if (mismatch_first) {
    struct packet *wrong = copy_packet(held->source, held->bytes, held->length);
    for (size_t c = 12; c + 4 <= wrong->length;) {
      size_t length = get16(wrong->bytes + c + 2);
      if (wrong->bytes[c] == 130)
        for (size_t t = c + 4; t + 4 <= c + length;) {
          size_t size = get16(wrong->bytes + t + 2);
          if (get16(wrong->bytes + t) == 13)
            put32(wrong->bytes + t + 8, reset_sequence - 1);
          t += (size + 3) & ~(size_t)3;
        }
      c += (length + 3) & ~(size_t)3;
    }
    checksum(wrong);
    usrsctp_conninput(&links[0], wrong->bytes, wrong->length, 0);
    free(wrong);
    pump();
    assert(links[0].outgoing_one == 0);
  }
  usrsctp_conninput(&links[0], held->bytes, held->length, 0);
  free(held);
  held = NULL;
  intercept = 0;
  pump();
  assert(links[0].outgoing_one == 1);
  send_message(sender, "lost", 1);
  assert(future_zero >= 1 && links[1].messages == 1);
  for (unsigned step = 0; step < 500 && !have_forward; step++) {
    struct timespec delay = {0, 10000000};
    nanosleep(&delay, NULL);
    usrsctp_handle_timers(10);
    pump();
  }
  assert(have_forward);
  send_message(sender, "reliable", 0);
  assert(future_one == 1 && links[1].messages == 2);
  printf("PASS mismatch_checked=%d old_ssn=0 mismatch_reset_events=0 "
         "matching_reset_events=1 "
         "expired_ssn=0 forward_ssn=0 reliable_ssn=1 exact_receipts=2\n",
         mismatch_first);
  struct linger abort_now = {1, 0};
  assert(usrsctp_setsockopt(sender, SOL_SOCKET, SO_LINGER, &abort_now,
                            sizeof(abort_now)) == 0);
  assert(usrsctp_setsockopt(receiver, SOL_SOCKET, SO_LINGER, &abort_now,
                            sizeof(abort_now)) == 0);
  usrsctp_close(sender);
  usrsctp_close(receiver);
  pump();
  for (int i = 0; i < 2; i++)
    usrsctp_deregister_address(&links[i]);
  assert(usrsctp_finish() == 0);
  return 0;
}
