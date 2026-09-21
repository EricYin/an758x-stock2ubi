// SPDX-License-Identifier: GPL-2.0-only
/*
 * Stock partition registration clears MTD_WRITEABLE on selected devices.
 * The bootstrap tool writes complete eraseblocks through mtdblock and reads
 * the resulting data through the MTD character device.
 *
 * The MTD core checks the partition flags before forwarding writes to the
 * parent NAND driver. Setting this bit retains the driver's ECC path.
 */

#include <linux/err.h>
#include <linux/module.h>
#include <linux/mtd/mtd.h>

#define AN758X_MTD_SCAN_LIMIT 64

typedef struct mtd_info *(*get_mtd_device_fn)(struct mtd_info *mtd, int num);
typedef void (*put_mtd_device_fn)(struct mtd_info *mtd);

/*
 * The executable resolves these MTD function addresses from /proc/kallsyms
 * and passes them as module parameters.
 */
static unsigned long get_mtd_device_addr;
static unsigned long put_mtd_device_addr;
module_param(get_mtd_device_addr, ulong, 0400);
module_param(put_mtd_device_addr, ulong, 0400);

static int __init an758x_mtd_rw_init(void)
{
	get_mtd_device_fn get_device;
	put_mtd_device_fn put_device;
	struct mtd_info *mtd;
	int changed = 0;
	int index;

	if (!get_mtd_device_addr || !put_mtd_device_addr)
		return -EINVAL;

	get_device = (get_mtd_device_fn)get_mtd_device_addr;
	put_device = (put_mtd_device_fn)put_mtd_device_addr;

	for (index = 0; index < AN758X_MTD_SCAN_LIMIT; index++) {
		mtd = get_device(NULL, index);
		if (IS_ERR(mtd))
			continue;

		if (!(mtd->flags & MTD_WRITEABLE)) {
			mtd->flags |= MTD_WRITEABLE;
			changed++;
			pr_info("an758x_mtd_rw: mtd%d (%s) is writable\n",
				index, mtd->name);
		}
		put_device(mtd);
	}

	pr_info("an758x_mtd_rw: enabled writes on %d MTD devices\n", changed);
	return 0;
}

module_init(an758x_mtd_rw_init);

MODULE_DESCRIPTION("Enable writes on AN758x MTD partitions");
MODULE_AUTHOR("AN758x Stock2UBI");
MODULE_LICENSE("GPL");
