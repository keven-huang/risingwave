// Copyright 2023 RisingWave Labs
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::ops::Bound;
use std::sync::Arc;

use risingwave_common::hash::VirtualNode;
use risingwave_hummock_sdk::compaction_group::hummock_version_ext::HummockLevelsExt;
use risingwave_hummock_sdk::key::{FullKey, UserKey};
use risingwave_hummock_sdk::key_range::KeyRangeCommon;
use risingwave_pb::hummock::hummock_version::Levels;
use risingwave_pb::hummock::{
    CompactionConfig, InputLevel, Level, LevelType, OverlappingLevel, SstableInfo,
};

use crate::hummock::compaction::create_overlap_strategy;
use crate::hummock::compaction::picker::{
    CompactionInput, CompactionPicker, LocalPickerStatistic, TrivialMovePicker,
};
use crate::hummock::level_handler::LevelHandler;

#[derive(Default, Clone, Debug)]
pub struct PartitionInfo {
    pub level_id: u32,
    pub sub_level_id: u64,
    pub left_idx: usize,
    pub right_idx: usize,
    pub total_file_size: u64,
}

#[derive(Default, Clone, Debug)]
pub struct SubLevelPartition {
    pub sub_levels: Vec<PartitionInfo>,
    pub total_file_size: u64,
}

pub struct IntraSubLevelPicker {
    config: Arc<CompactionConfig>,
    partitions: Vec<SubLevelPartition>,
}

impl IntraSubLevelPicker {
    pub fn new(config: Arc<CompactionConfig>, partitions: Vec<SubLevelPartition>) -> Self {
        Self { config, partitions }
    }

    fn pick_l0_trivial_move_file(
        &self,
        l0: &OverlappingLevel,
        level_handlers: &[LevelHandler],
        stats: &mut LocalPickerStatistic,
    ) -> Option<CompactionInput> {
        let overlap_strategy = create_overlap_strategy(self.config.compaction_mode());

        for (idx, level) in l0.sub_levels.iter().enumerate() {
            if level.level_type == LevelType::Overlapping as i32 || idx + 1 >= l0.sub_levels.len() {
                continue;
            }

            if l0.sub_levels[idx + 1].level_type == LevelType::Overlapping as i32 {
                continue;
            }

            let trivial_move_picker = TrivialMovePicker::new(0, 0, overlap_strategy.clone());

            let select_sst = trivial_move_picker.pick_trivial_move_sst(
                &l0.sub_levels[idx + 1].table_infos,
                &level.table_infos,
                level_handlers,
                stats,
            );

            // only pick tables for trivial move
            if select_sst.is_none() {
                continue;
            }

            let select_sst = select_sst.unwrap();

            // support trivial move cross multi sub_levels
            let mut overlap = overlap_strategy.create_overlap_info();
            overlap.update(&select_sst);

            assert!(overlap
                .check_multiple_overlap(&l0.sub_levels[idx].table_infos)
                .is_empty());
            let mut target_level_idx = idx;
            while target_level_idx > 0 {
                if l0.sub_levels[target_level_idx - 1].level_type
                    != LevelType::Nonoverlapping as i32
                    || !overlap
                        .check_multiple_overlap(&l0.sub_levels[target_level_idx - 1].table_infos)
                        .is_empty()
                {
                    break;
                }
                target_level_idx -= 1;
            }

            let input_levels = vec![
                InputLevel {
                    level_idx: 0,
                    level_type: LevelType::Nonoverlapping as i32,
                    table_infos: vec![select_sst],
                },
                InputLevel {
                    level_idx: 0,
                    level_type: LevelType::Nonoverlapping as i32,
                    table_infos: vec![],
                },
            ];
            return Some(CompactionInput {
                input_levels,
                target_level: 0,
                target_sub_level_id: l0.sub_levels[target_level_idx].sub_level_id,
                vnode_partition_count: 0,
            });
        }
        None
    }
}

impl CompactionPicker for IntraSubLevelPicker {
    fn pick_compaction(
        &mut self,
        levels: &Levels,
        level_handlers: &[LevelHandler],
        stats: &mut LocalPickerStatistic,
    ) -> Option<CompactionInput> {
        let l0 = levels.l0.as_ref().unwrap();
        let max_sub_level_id = self
            .partitions
            .iter()
            .map(|partition| {
                partition
                    .sub_levels
                    .last()
                    .map(|level| level.sub_level_id)
                    .unwrap_or(u64::MAX)
            })
            .min()
            .unwrap_or(0);
        println!("max_sub_level_id: {}", max_sub_level_id);

        for (idx, level) in l0.sub_levels.iter().enumerate() {
            if level.level_type() != LevelType::Nonoverlapping
                || level_handlers[0].is_level_pending_compact(level)
            {
                continue;
            }

            if levels.vnode_partition_count == 0
                && level.total_file_size > self.config.sub_level_max_compaction_bytes / 2
            {
                continue;
            }

            if level.vnode_partition_count > 0 && level.sub_level_id <= max_sub_level_id {
                continue;
            }

            let max_compaction_bytes = std::cmp::min(
                self.config.max_compaction_bytes,
                self.config.sub_level_max_compaction_bytes,
            );

            let mut compaction_bytes = 0;
            let mut compaction_file_count = 0;
            let mut input_levels = vec![];
            if l0.sub_levels.len() - idx < self.config.level0_sub_level_compact_level_count as usize
            {
                continue;
            }

            let mut wait_enough = true;
            for next_level in l0.sub_levels.iter().skip(idx) {
                if compaction_file_count >= self.config.level0_max_compact_file_number
                    || compaction_bytes >= max_compaction_bytes
                {
                    break;
                }

                if level.vnode_partition_count == 0 && next_level.vnode_partition_count > 0 {
                    break;
                }

                if level_handlers[0].is_level_pending_compact(next_level) {
                    wait_enough = false;
                    break;
                }

                compaction_bytes += next_level.total_file_size;
                compaction_file_count += next_level.table_infos.len() as u64;

                input_levels.push(InputLevel {
                    level_idx: 0,
                    level_type: next_level.level_type,
                    table_infos: next_level.table_infos.clone(),
                });
            }

            if input_levels.len() < self.config.level0_sub_level_compact_level_count as usize
                && (!levels.can_partition_by_vnode()
                    || !wait_enough
                    || level.vnode_partition_count > 0)
            {
                continue;
            }
            input_levels.reverse();

            let vnode_partition_count = if compaction_bytes >= max_compaction_bytes {
                levels.vnode_partition_count
            } else {
                0
            };

            return Some(CompactionInput {
                input_levels,
                target_level: 0,
                target_sub_level_id: level.sub_level_id,
                vnode_partition_count,
            });
        }

        let mut skip_pending_compact = false;

        for part in &self.partitions {
            for (idx, info) in part.sub_levels.iter().enumerate() {
                if info.total_file_size > self.config.sub_level_max_compaction_bytes / 2 {
                    continue;
                }

                let max_compaction_bytes = std::cmp::min(
                    self.config.max_compaction_bytes,
                    self.config.max_bytes_for_level_base,
                );

                let mut compaction_bytes = 0;
                let mut compaction_file_count = 0;
                let mut input_levels = vec![];
                for right in idx..part.sub_levels.len() {
                    if compaction_file_count > self.config.level0_max_compact_file_number
                        || compaction_bytes > max_compaction_bytes
                    {
                        break;
                    }
                    let mut pending_compact = false;
                    let sub_level_info = &part.sub_levels[right];
                    if compaction_bytes > self.config.sub_level_max_compaction_bytes
                        && sub_level_info.total_file_size
                            > self.config.sub_level_max_compaction_bytes / 2
                    {
                        break;
                    }
                    if sub_level_info.right_idx > sub_level_info.left_idx {
                        let mut input_level = InputLevel {
                            level_idx: 0,
                            level_type: l0.sub_levels[right].level_type,
                            table_infos: Vec::with_capacity(
                                sub_level_info.right_idx - sub_level_info.left_idx,
                            ),
                        };
                        for sst in &l0.sub_levels[right].table_infos
                            [sub_level_info.left_idx..sub_level_info.right_idx]
                        {
                            if level_handlers[0].is_pending_compact(&sst.sst_id) {
                                pending_compact = true;
                                break;
                            }
                            compaction_bytes += sst.file_size;
                            input_level.table_infos.push(sst.clone());
                        }
                        compaction_file_count +=
                            (sub_level_info.right_idx - sub_level_info.left_idx) as u64;
                        input_levels.push(input_level);
                    }
                    if pending_compact {
                        skip_pending_compact = true;
                        break;
                    }
                }

                if input_levels.len() < self.config.level0_sub_level_compact_level_count as usize {
                    continue;
                }

                stats.use_vnode_partition = true;

                return Some(CompactionInput {
                    input_levels,
                    target_level: 0,
                    target_sub_level_id: l0.sub_levels[idx].sub_level_id,
                    vnode_partition_count: levels.vnode_partition_count,
                });
            }
        }
        if skip_pending_compact {
            stats.skip_by_pending_files += 1;
        } else {
            stats.skip_by_count_limit += 1;
        }
        self.pick_l0_trivial_move_file(levels.l0.as_ref().unwrap(), level_handlers, stats)
    }
}

pub fn partition_sub_levels(levels: &Levels) -> Vec<SubLevelPartition> {
    if levels.member_table_ids.len() != 1 || levels.vnode_partition_count == 0 {
        return vec![];
    }

    let mut partition_vnode_count: usize = 1;
    while partition_vnode_count * 2 <= (levels.vnode_partition_count as usize) {
        partition_vnode_count *= 2;
    }
    let mut partitions = Vec::with_capacity(partition_vnode_count);
    for _ in 0..partition_vnode_count {
        partitions.push(SubLevelPartition::default());
    }
    for level in &levels.l0.as_ref().unwrap().sub_levels {
        if level.level_type() != LevelType::Nonoverlapping || level.vnode_partition_count == 0 {
            break;
        }
        assert_eq!(levels.member_table_ids.len(), 1);
        if !partition_level(
            levels.member_table_ids[0],
            partition_vnode_count,
            level,
            &mut partitions,
        ) {
            break;
        }
    }
    partitions
}

pub fn can_partition_level(
    table_id: u32,
    partition_vnode_count: usize,
    table_infos: &[SstableInfo],
) -> bool {
    let mut left_idx = 0;
    let mut can_partition = true;
    let partition_size = VirtualNode::COUNT / partition_vnode_count;
    for partition_id in 0..partition_vnode_count {
        let smallest_vnode = partition_id * partition_size;
        let largest_vnode = (partition_id + 1) * partition_size;
        let smallest_table_key =
            UserKey::prefix_of_vnode(table_id, VirtualNode::from_index(smallest_vnode));
        let largest_table_key = if largest_vnode >= VirtualNode::COUNT {
            Bound::Unbounded
        } else {
            Bound::Excluded(UserKey::prefix_of_vnode(
                table_id,
                VirtualNode::from_index(largest_vnode),
            ))
        };
        while left_idx < table_infos.len() {
            let key_range = table_infos[left_idx].key_range.as_ref().unwrap();
            let ret = key_range.compare_right_with_user_key(smallest_table_key.as_ref());
            if ret != std::cmp::Ordering::Less {
                break;
            }
            left_idx += 1;
        }
        if left_idx >= table_infos.len() {
            return true;
        }

        if FullKey::decode(&table_infos[left_idx].key_range.as_ref().unwrap().left)
            .user_key
            .lt(&smallest_table_key.as_ref())
        {
            can_partition = false;
            break;
        }
        let mut right_idx = left_idx;
        while right_idx < table_infos.len() {
            let key_range = table_infos[right_idx].key_range.as_ref().unwrap();
            let ret = match &largest_table_key {
                Bound::Excluded(key) => key_range.compare_right_with_user_key(key.as_ref()),
                Bound::Unbounded => {
                    let right_key = FullKey::decode(&key_range.right);
                    assert!(right_key.user_key.table_id.table_id == table_id);
                    // We would assign partition_vnode_count to a level only when we compact all
                    // sstable of it, so there will never be another stale table in this sstable
                    // file.
                    std::cmp::Ordering::Less
                }
                _ => unreachable!(),
            };

            if ret != std::cmp::Ordering::Less {
                break;
            }
            right_idx += 1;
        }

        if right_idx < table_infos.len()
            && match &largest_table_key {
                Bound::Excluded(key) => {
                    FullKey::decode(&table_infos[right_idx].key_range.as_ref().unwrap().left)
                        .user_key
                        .lt(&key.as_ref())
                }
                _ => unreachable!(),
            }
        {
            can_partition = false;
            break;
        }
        left_idx = right_idx;
    }

    can_partition
}

pub fn partition_level(
    table_id: u32,
    partition_vnode_count: usize,
    level: &Level,
    partitions: &mut Vec<SubLevelPartition>,
) -> bool {
    assert_eq!(partition_vnode_count, partitions.len());
    let mut left_idx = 0;
    let mut can_partition = true;
    let partition_size = VirtualNode::COUNT / partition_vnode_count;
    for (partition_id, partition) in partitions.iter_mut().enumerate() {
        let smallest_vnode = partition_id * partition_size;
        let largest_vnode = (partition_id + 1) * partition_size;
        let smallest_table_key =
            UserKey::prefix_of_vnode(table_id, VirtualNode::from_index(smallest_vnode));
        let largest_table_key = if largest_vnode >= VirtualNode::COUNT {
            Bound::Unbounded
        } else {
            Bound::Excluded(UserKey::prefix_of_vnode(
                table_id,
                VirtualNode::from_index(largest_vnode),
            ))
        };
        while left_idx < level.table_infos.len() {
            let key_range = level.table_infos[left_idx].key_range.as_ref().unwrap();
            let ret = key_range.compare_right_with_user_key(smallest_table_key.as_ref());
            if ret != std::cmp::Ordering::Less {
                break;
            }
            left_idx += 1;
        }
        if left_idx >= level.table_infos.len() {
            partition.sub_levels.push(PartitionInfo {
                sub_level_id: level.sub_level_id,
                left_idx: 0,
                right_idx: 0,
                total_file_size: 0,
                level_id: level.level_idx,
            });
            continue;
        }

        if FullKey::decode(&level.table_infos[left_idx].key_range.as_ref().unwrap().left)
            .user_key
            .lt(&smallest_table_key.as_ref())
        {
            can_partition = false;
            break;
        }
        let mut total_file_size = 0;
        let mut right_idx = left_idx;
        while right_idx < level.table_infos.len() {
            let key_range = level.table_infos[right_idx].key_range.as_ref().unwrap();
            let ret = match &largest_table_key {
                Bound::Excluded(key) => key_range.compare_right_with_user_key(key.as_ref()),
                Bound::Unbounded => {
                    let right_key = FullKey::decode(&key_range.right);
                    assert!(right_key.user_key.table_id.table_id == table_id);
                    // We would assign partition_vnode_count to a level only when we compact all
                    // sstable of it, so there will never be another stale table in this sstable
                    // file.
                    std::cmp::Ordering::Less
                }
                _ => unreachable!(),
            };

            if ret != std::cmp::Ordering::Less {
                break;
            }
            total_file_size += level.table_infos[right_idx].file_size;
            right_idx += 1;
        }

        if right_idx < level.table_infos.len()
            && match &largest_table_key {
                Bound::Excluded(key) => FullKey::decode(
                    &level.table_infos[right_idx]
                        .key_range
                        .as_ref()
                        .unwrap()
                        .left,
                )
                .user_key
                .lt(&key.as_ref()),
                _ => unreachable!(),
            }
        {
            can_partition = false;
            break;
        }
        partition.total_file_size += total_file_size;
        partition.sub_levels.push(PartitionInfo {
            sub_level_id: level.sub_level_id,
            left_idx,
            right_idx,
            total_file_size,
            level_id: level.level_idx,
        });
        left_idx = right_idx;
    }

    if !can_partition {
        for partition in partitions {
            if let Some(last_info) = partition.sub_levels.last() {
                if level.sub_level_id == last_info.sub_level_id
                    && level.level_idx == last_info.level_id
                {
                    partition.sub_levels.pop();
                }
            }
        }
    }
    can_partition
}