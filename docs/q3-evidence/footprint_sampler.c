/* footprint_sampler.c —— q3 空载内存门禁采样器（proc_pid_rusage 同款口径）。
 *
 * 用途：对一个 pid 采 N 个 ri_phys_footprint 样本（间隔秒），输出中位数
 * （字节）。与宿主 plugins.rs 的 footprint_bytes 同一坑位纪律：
 *   - 内核按**当前** flavor 的完整结构体写穿（v6 = 16B uuid + 31×u64），
 *     缓冲必须给 v6 全尺寸，短了会被内核写穿（旧树实测 SIGBUS）；
 *   - ri_phys_footprint 在公共前缀（uuid[16] + 7×u64 之后，偏移 72），
 *     flavor 用 V4 只读前缀字段，偏移由 ABI 钉死。
 *
 * 编译: clang -O2 docs/q3-evidence/footprint_sampler.c -o /tmp/nq3-sampler
 * 用法: footprint_sampler <pid> <samples> <interval_ms>
 * 输出: 单行中位数（字节）。
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <libproc.h>

/* 缓冲尺寸：内核按**当前** flavor 的完整结构体写穿（v6 = 16B uuid +
 * 31×u64 = 264B；本机内核实测写得更宽——264B 缓冲在退出期触发
 * stack-protector abort），固定给 512B 裕量。 */
#define RI_BUF_BYTES 512
#define RI_PHYS_FOOTPRINT_OFF (16 + 7 * 8)
#define RUSAGE_INFO_V4 4

static int cmp_u64(const void *a, const void *b) {
    unsigned long long x = *(const unsigned long long *)a;
    unsigned long long y = *(const unsigned long long *)b;
    return x < y ? -1 : (x > y ? 1 : 0);
}

int main(int argc, char **argv) {
    if (argc != 4) {
        fprintf(stderr, "usage: %s <pid> <samples> <interval_ms>\n", argv[0]);
        return 2;
    }
    int pid = atoi(argv[1]);
    int n = atoi(argv[2]);
    int ms = atoi(argv[3]);
    if (pid <= 0 || n <= 0) {
        fprintf(stderr, "bad args\n");
        return 2;
    }
    unsigned long long *vals = calloc((size_t)n, sizeof(unsigned long long));
    for (int i = 0; i < n; i++) {
        unsigned char info[RI_BUF_BYTES];
        memset(info, 0, sizeof(info));
        int r = proc_pid_rusage(pid, RUSAGE_INFO_V4, (void *)info);
        if (r != 0) {
            fprintf(stderr, "proc_pid_rusage(%d) failed\n", pid);
            return 1;
        }
        unsigned long long v = 0;
        memcpy(&v, info + RI_PHYS_FOOTPRINT_OFF, 8);
        vals[i] = v;
        if (i + 1 < n) {
            usleep((useconds_t)ms * 1000);
        }
    }
    qsort(vals, (size_t)n, sizeof(unsigned long long), cmp_u64);
    printf("%llu\n", vals[n / 2]);
    return 0;
}
