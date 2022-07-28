use super::{
    DeleteAllRelationsInternalInput, DeleteMultipleRelationsInternalInput, DeleteNodeConstraintInternalInput,
    DeleteNodeInternalInput, DeleteRelationInternalInput, DeleteUnitNodeConstraintInput, ExecuteChangesOnDatabase,
    InsertNodeInternalInput, InsertRelationInternalInput, InternalChanges, InternalNodeChanges,
    InternalNodeConstraintChanges, InternalRelationChanges, ToTransactionError, ToTransactionFuture,
    UpdateNodeInternalInput, UpdateRelation, UpdateRelationInternalInput,
};
use crate::constant;
use crate::model::constraint::{ConstraintDefinition, ConstraintType};
use crate::TxItem;
use crate::{DynamoDBBatchersData, DynamoDBContext};
use chrono::Utc;
use dynomite::Attribute;
use rusoto_dynamodb::{Delete, Put, TransactWriteItem, Update};
use std::collections::HashMap;

impl ExecuteChangesOnDatabase for InsertNodeInternalInput {
    fn to_transaction<'a>(
        self,
        batchers: &'a DynamoDBBatchersData,
        ctx: &'a DynamoDBContext,
        pk: String,
        sk: String,
    ) -> ToTransactionFuture<'a> {
        Box::pin(async {
            let InsertNodeInternalInput {
                id,
                mut user_defined_item,
                ty,
                constraints,
            } = self;

            let id = format!("{}#{}", ty, id);
            let now_attr = Utc::now().to_string().into_attr();
            let ty_attr = ty.clone().into_attr();
            let autogenerated_id_attr = id.into_attr();

            user_defined_item.insert(constant::PK.to_string(), autogenerated_id_attr.clone());
            user_defined_item.insert(constant::SK.to_string(), autogenerated_id_attr.clone());

            user_defined_item.insert(constant::TYPE.to_string(), ty_attr.clone());

            user_defined_item.insert(constant::CREATED_AT.to_string(), now_attr.clone());
            user_defined_item.insert(constant::UPDATED_AT.to_string(), now_attr);

            user_defined_item.insert(constant::TYPE_INDEX_PK.to_string(), ty_attr);
            user_defined_item.insert(constant::TYPE_INDEX_SK.to_string(), autogenerated_id_attr.clone());

            user_defined_item.insert(constant::INVERTED_INDEX_PK.to_string(), autogenerated_id_attr.clone());
            user_defined_item.insert(constant::INVERTED_INDEX_SK.to_string(), autogenerated_id_attr);

            let mut node_transaction = vec![];

            for ConstraintDefinition {
                field,
                r#type: ConstraintType::Unique,
            } in &constraints
            {
                // FIXME: unique_value_serialised()
                let value = serde_json::to_string(&user_defined_item[field]).expect("must be a valid JSON");
                let unique_column_pk_sk = format!("__C#{ty}#{field}#{value}");
                node_transaction.push(TxItem {
                    pk: unique_column_pk_sk.clone(),
                    sk: unique_column_pk_sk.clone(),
                    relation_name: None,
                    transaction: TransactWriteItem {
                        put: Some(Put {
                            table_name: ctx.dynamodb_table_name.clone(),
                            item: dynomite::attr_map! {
                                constant::PK => unique_column_pk_sk.clone(),
                                constant::SK => unique_column_pk_sk.clone(),
                                constant::ITEM_PK => pk.clone(),
                                constant::INVERTED_INDEX_PK => pk.clone(),
                                constant::INVERTED_INDEX_SK => unique_column_pk_sk,
                            },
                            condition_expression: Some("attribute_not_exists(#pk)".to_string()),
                            expression_attribute_names: Some(HashMap::from([(
                                "#pk".to_string(),
                                constant::PK.to_string(),
                            )])),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                });
            }

            node_transaction.push(TxItem {
                pk,
                sk,
                relation_name: None,
                transaction: TransactWriteItem {
                    put: Some(Put {
                        table_name: ctx.dynamodb_table_name.clone(),
                        item: user_defined_item,
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            });

            batchers
                .transaction
                .load_many(node_transaction)
                .await
                .map_err(ToTransactionError::TransactionError)
        })
    }
}

impl ExecuteChangesOnDatabase for UpdateNodeInternalInput {
    fn to_transaction<'a>(
        self,
        batchers: &'a DynamoDBBatchersData,
        ctx: &'a DynamoDBContext,
        pk: String,
        sk: String,
    ) -> ToTransactionFuture<'a> {
        Box::pin(async {
            let UpdateNodeInternalInput {
                id,
                mut user_defined_item,
                ty,
                constraints,
            } = self;

            let id = format!("{}#{}", id, ty);
            let now_attr = Utc::now().to_string().into_attr();
            let ty_attr = ty.clone().into_attr();
            let autogenerated_id_attr = id.into_attr();
            let len = user_defined_item.len();

            user_defined_item.insert(constant::PK.to_string(), autogenerated_id_attr.clone());
            user_defined_item.insert(constant::SK.to_string(), autogenerated_id_attr.clone());

            user_defined_item.insert(constant::TYPE.to_string(), ty_attr.clone());

            user_defined_item.insert(constant::CREATED_AT.to_string(), now_attr.clone());
            user_defined_item.insert(constant::UPDATED_AT.to_string(), now_attr);

            user_defined_item.insert(constant::TYPE_INDEX_PK.to_string(), ty_attr);
            user_defined_item.insert(constant::TYPE_INDEX_SK.to_string(), autogenerated_id_attr.clone());

            user_defined_item.insert(constant::INVERTED_INDEX_PK.to_string(), autogenerated_id_attr.clone());
            user_defined_item.insert(constant::INVERTED_INDEX_SK.to_string(), autogenerated_id_attr);

            let mut node_transaction = vec![];

            // FIXME: Generalise once we have more kinds of constraints.
            for ConstraintDefinition {
                field,
                r#type: ConstraintType::Unique,
            } in &constraints
            {
                if !user_defined_item.contains_key(field) {
                    continue;
                }

                let value = serde_json::to_string(&user_defined_item[field]).expect("must be a valid JSON");
                let unique_column_pk_sk = format!("__C#{ty}#{field}#{value}");

                node_transaction.push(TxItem {
                    pk: unique_column_pk_sk.clone(),
                    sk: unique_column_pk_sk.clone(),
                    relation_name: None,
                    transaction: TransactWriteItem {
                        update: Some(Update {
                            table_name: ctx.dynamodb_table_name.clone(),
                            key: dynomite::attr_map! {
                                constant::PK => unique_column_pk_sk.clone(),
                                constant::SK => unique_column_pk_sk,
                            },
                            condition_expression: Some("attribute_not_exists(#pk) OR #item_pk = :item_pk".to_string()),
                            update_expression: "SET #item_pk = :item_pk".to_string(),
                            expression_attribute_names: Some(HashMap::from([
                                ("#pk".to_string(), constant::PK.to_string()),
                                ("#item_pk".to_string(), constant::ITEM_PK.to_string()),
                            ])),
                            expression_attribute_values: Some(dynomite::attr_map! {
                                ":item_pk" => pk.clone(),
                            }),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                });
            }

            let mut exp_values = HashMap::with_capacity(len);
            let mut exp_att_names = HashMap::from([
                ("#pk".to_string(), "__pk".to_string()),
                ("#sk".to_string(), "__sk".to_string()),
            ]);
            let update_expression = Self::to_update_expression(user_defined_item, &mut exp_values, &mut exp_att_names);
            let key = dynomite::attr_map! {
                constant::PK => pk.clone(),
                constant::SK => sk.clone(),
            };

            let update_transaction: TransactWriteItem = TransactWriteItem {
                update: Some(Update {
                    table_name: ctx.dynamodb_table_name.clone(),
                    key,
                    condition_expression: Some("attribute_exists(#pk) AND attribute_exists(#sk)".to_string()),
                    update_expression,
                    expression_attribute_values: Some(exp_values),
                    expression_attribute_names: Some(exp_att_names),
                    ..Default::default()
                }),
                ..Default::default()
            };

            node_transaction.push(TxItem {
                pk,
                sk,
                relation_name: None,
                transaction: update_transaction,
            });

            batchers
                .transaction
                .load_many(node_transaction)
                .await
                .map_err(ToTransactionError::TransactionError)
        })
    }
}
impl ExecuteChangesOnDatabase for DeleteNodeInternalInput {
    fn to_transaction<'a>(
        self,
        batchers: &'a DynamoDBBatchersData,
        ctx: &'a DynamoDBContext,
        pk: String,
        sk: String,
    ) -> ToTransactionFuture<'a> {
        Box::pin(async {
            let DeleteNodeInternalInput { .. } = self;

            let key = dynomite::attr_map! {
                    constant::PK => pk.clone(),
                    constant::SK => sk.clone(),
            };

            let exp_att_names = HashMap::from([
                ("#pk".to_string(), constant::PK.to_string()),
                ("#sk".to_string(), constant::SK.to_string()),
            ]);

            let delete_transaction = Delete {
                table_name: ctx.dynamodb_table_name.clone(),
                condition_expression: Some("attribute_exists(#pk) AND attribute_exists(#sk)".to_string()),
                key,
                expression_attribute_names: Some(exp_att_names),
                ..Default::default()
            };

            let node_transaction = TxItem {
                pk,
                sk,
                relation_name: None,
                transaction: TransactWriteItem {
                    delete: Some(delete_transaction),
                    ..Default::default()
                },
            };

            batchers
                .transaction
                .load_many(vec![node_transaction])
                .await
                .map_err(ToTransactionError::TransactionError)
        })
    }
}

impl ExecuteChangesOnDatabase for InternalNodeChanges {
    fn to_transaction<'a>(
        self,
        batchers: &'a DynamoDBBatchersData,
        ctx: &'a DynamoDBContext,
        pk: String,
        sk: String,
    ) -> ToTransactionFuture<'a> {
        match self {
            Self::Insert(a) => a.to_transaction(batchers, ctx, pk, sk),
            Self::Delete(a) => a.to_transaction(batchers, ctx, pk, sk),
            Self::Update(a) => a.to_transaction(batchers, ctx, pk, sk),
        }
    }
}

impl ExecuteChangesOnDatabase for InsertRelationInternalInput {
    fn to_transaction<'a>(
        self,
        batchers: &'a DynamoDBBatchersData,
        ctx: &'a DynamoDBContext,
        pk: String,
        sk: String,
    ) -> ToTransactionFuture<'a> {
        Box::pin(async {
            let InsertRelationInternalInput {
                mut fields,
                relation_names,
                from_ty,
                to_ty,
                ..
            } = self;

            let now_attr = Utc::now().to_string().into_attr();
            let gsi1pk_attr = from_ty.into_attr();
            let ty_attr = to_ty.into_attr();
            let partition_key_attr = pk.clone().into_attr();
            let sorting_key_attr = sk.clone().into_attr();

            fields.remove(constant::PK);
            fields.remove(constant::SK);

            fields.insert(constant::TYPE.to_string(), ty_attr.clone());

            fields.insert(constant::UPDATED_AT.to_string(), now_attr);

            fields.insert(constant::TYPE_INDEX_PK.to_string(), gsi1pk_attr);
            fields.insert(constant::TYPE_INDEX_SK.to_string(), partition_key_attr.clone());

            fields.insert(constant::INVERTED_INDEX_PK.to_string(), sorting_key_attr);
            fields.insert(constant::INVERTED_INDEX_SK.to_string(), partition_key_attr);

            let mut exp_values = HashMap::with_capacity(fields.len() + 1);
            let mut exp_att_names = HashMap::with_capacity(fields.len() + 1);
            let update_expression = UpdateRelationInternalInput::to_update_expression(
                fields,
                &mut exp_values,
                &mut exp_att_names,
                relation_names.into_iter().map(UpdateRelation::Add).collect(),
                true,
            );

            let key = dynomite::attr_map! {
                    constant::PK => pk.clone(),
                    constant::SK => sk.clone(),
            };

            let update_transaction: TransactWriteItem = TransactWriteItem {
                update: Some(Update {
                    table_name: ctx.dynamodb_table_name.clone(),
                    key,
                    update_expression,
                    expression_attribute_values: Some(exp_values),
                    expression_attribute_names: Some(exp_att_names),
                    ..Default::default()
                }),
                ..Default::default()
            };

            let node_transaction = TxItem {
                pk,
                sk,
                relation_name: None,
                transaction: update_transaction,
            };

            batchers
                .transaction
                .load_many(vec![node_transaction])
                .await
                .map_err(ToTransactionError::TransactionError)
        })
    }
}

impl ExecuteChangesOnDatabase for DeleteAllRelationsInternalInput {
    fn to_transaction<'a>(
        self,
        batchers: &'a DynamoDBBatchersData,
        ctx: &'a DynamoDBContext,
        pk: String,
        sk: String,
    ) -> ToTransactionFuture<'a> {
        Box::pin(async {
            let key = dynomite::attr_map! {
                    constant::PK => pk.clone(),
                    constant::SK => sk.clone(),
            };

            let exp_att_names = HashMap::from([
                ("#pk".to_string(), "__pk".to_string()),
                ("#sk".to_string(), "__sk".to_string()),
            ]);

            let delete_transaction = Delete {
                table_name: ctx.dynamodb_table_name.clone(),
                condition_expression: Some("attribute_exists(#pk) AND attribute_exists(#sk)".to_string()),
                expression_attribute_names: Some(exp_att_names),
                key,
                ..Default::default()
            };

            let node_transaction = TxItem {
                pk,
                sk,
                relation_name: None,
                transaction: TransactWriteItem {
                    delete: Some(delete_transaction),
                    ..Default::default()
                },
            };

            batchers
                .transaction
                .load_many(vec![node_transaction])
                .await
                .map_err(ToTransactionError::TransactionError)
        })
    }
}

impl ExecuteChangesOnDatabase for DeleteMultipleRelationsInternalInput {
    fn to_transaction<'a>(
        self,
        batchers: &'a DynamoDBBatchersData,
        ctx: &'a DynamoDBContext,
        pk: String,
        sk: String,
    ) -> ToTransactionFuture<'a> {
        Box::pin(async {
            let DeleteMultipleRelationsInternalInput { relation_names, .. } = self;

            let now_attr = Utc::now().to_string().into_attr();

            let mut user_defined_item = HashMap::with_capacity(1);
            user_defined_item.insert(constant::UPDATED_AT.to_string(), now_attr);

            let mut exp_values = HashMap::with_capacity(16);
            let mut exp_att_names = HashMap::with_capacity(user_defined_item.len() + 1);

            let update_expression = UpdateRelationInternalInput::to_update_expression(
                user_defined_item,
                &mut exp_values,
                &mut exp_att_names,
                relation_names.into_iter().map(UpdateRelation::Remove).collect(),
                false,
            );
            let key = dynomite::attr_map! {
                    constant::PK => pk.clone(),
                    constant::SK => sk.clone(),
            };

            let update_transaction: TransactWriteItem = TransactWriteItem {
                update: Some(Update {
                    table_name: ctx.dynamodb_table_name.clone(),
                    key,
                    update_expression,
                    expression_attribute_values: Some(exp_values),
                    expression_attribute_names: Some(exp_att_names),
                    ..Default::default()
                }),
                ..Default::default()
            };

            let node_transaction = TxItem {
                pk,
                sk,
                relation_name: None,
                transaction: update_transaction,
            };

            batchers
                .transaction
                .load_many(vec![node_transaction])
                .await
                .map_err(ToTransactionError::TransactionError)
        })
    }
}

impl ExecuteChangesOnDatabase for DeleteRelationInternalInput {
    fn to_transaction<'a>(
        self,
        batchers: &'a DynamoDBBatchersData,
        ctx: &'a DynamoDBContext,
        pk: String,
        sk: String,
    ) -> ToTransactionFuture<'a> {
        match self {
            Self::All(a) => a.to_transaction(batchers, ctx, pk, sk),
            Self::Multiple(a) => a.to_transaction(batchers, ctx, pk, sk),
        }
    }
}

impl ExecuteChangesOnDatabase for UpdateRelationInternalInput {
    fn to_transaction<'a>(
        self,
        batchers: &'a DynamoDBBatchersData,
        ctx: &'a DynamoDBContext,
        pk: String,
        sk: String,
    ) -> ToTransactionFuture<'a> {
        Box::pin(async {
            let UpdateRelationInternalInput {
                mut user_defined_item,
                relation_names,
                ..
            } = self;

            let now_attr = Utc::now().to_string().into_attr();
            user_defined_item.insert(constant::UPDATED_AT.to_string(), now_attr);

            let mut exp_values = HashMap::with_capacity(user_defined_item.len() + 1);
            let mut exp_att_names = HashMap::with_capacity(user_defined_item.len() + 1);
            let update_expression = Self::to_update_expression(
                user_defined_item,
                &mut exp_values,
                &mut exp_att_names,
                relation_names,
                false,
            );

            let key = dynomite::attr_map! {
                    constant::PK => pk.clone(),
                    constant::SK => sk.clone(),
            };

            let update_transaction: TransactWriteItem = TransactWriteItem {
                update: Some(Update {
                    table_name: ctx.dynamodb_table_name.clone(),
                    key,
                    update_expression,
                    expression_attribute_values: Some(exp_values),
                    expression_attribute_names: Some(exp_att_names),
                    ..Default::default()
                }),
                ..Default::default()
            };

            let node_transaction = TxItem {
                pk,
                sk,
                relation_name: None,
                transaction: update_transaction,
            };

            batchers
                .transaction
                .load_many(vec![node_transaction])
                .await
                .map_err(ToTransactionError::TransactionError)
        })
    }
}

impl ExecuteChangesOnDatabase for DeleteUnitNodeConstraintInput {
    fn to_transaction<'a>(
        self,
        batchers: &'a DynamoDBBatchersData,
        ctx: &'a DynamoDBContext,
        pk: String,
        sk: String,
    ) -> ToTransactionFuture<'a> {
        Box::pin(async {
            let key = dynomite::attr_map! {
                    constant::PK => pk.clone(),
                    constant::SK => sk.clone(),
            };

            let exp_att_names = HashMap::from([
                ("#pk".to_string(), constant::PK.to_string()),
                ("#sk".to_string(), constant::SK.to_string()),
            ]);

            let delete_transaction = Delete {
                table_name: ctx.dynamodb_table_name.clone(),
                condition_expression: Some("attribute_exists(#pk) AND attribute_exists(#sk)".to_string()),
                key,
                expression_attribute_names: Some(exp_att_names),
                ..Default::default()
            };

            let node_transaction = TxItem {
                pk,
                sk,
                relation_name: None,
                transaction: TransactWriteItem {
                    delete: Some(delete_transaction),
                    ..Default::default()
                },
            };

            batchers
                .transaction
                .load_many(vec![node_transaction])
                .await
                .map_err(ToTransactionError::TransactionError)
        })
    }
}

impl ExecuteChangesOnDatabase for DeleteNodeConstraintInternalInput {
    fn to_transaction<'a>(
        self,
        batchers: &'a DynamoDBBatchersData,
        ctx: &'a DynamoDBContext,
        pk: String,
        sk: String,
    ) -> ToTransactionFuture<'a> {
        match self {
            Self::Unit(a) => a.to_transaction(batchers, ctx, pk, sk),
        }
    }
}

impl ExecuteChangesOnDatabase for InternalNodeConstraintChanges {
    fn to_transaction<'a>(
        self,
        batchers: &'a DynamoDBBatchersData,
        ctx: &'a DynamoDBContext,
        pk: String,
        sk: String,
    ) -> ToTransactionFuture<'a> {
        match self {
            Self::Delete(a) => a.to_transaction(batchers, ctx, pk, sk),
        }
    }
}

impl ExecuteChangesOnDatabase for InternalRelationChanges {
    fn to_transaction<'a>(
        self,
        batchers: &'a DynamoDBBatchersData,
        ctx: &'a DynamoDBContext,
        pk: String,
        sk: String,
    ) -> ToTransactionFuture<'a> {
        match self {
            Self::Insert(a) => a.to_transaction(batchers, ctx, pk, sk),
            Self::Delete(a) => a.to_transaction(batchers, ctx, pk, sk),
            Self::Update(a) => a.to_transaction(batchers, ctx, pk, sk),
        }
    }
}

impl ExecuteChangesOnDatabase for Vec<InternalChanges> {
    fn to_transaction<'a>(
        self,
        batchers: &'a DynamoDBBatchersData,
        ctx: &'a DynamoDBContext,
        pk: String,
        sk: String,
    ) -> ToTransactionFuture<'a> {
        let mut list = self.into_iter();
        let first = list.next().map(|first| list.try_fold(first, |acc, cur| acc.with(cur)));

        let first = match first {
            Some(Ok(first)) => first,
            _ => {
                return Box::pin(async { Err(ToTransactionError::Unknown) });
            }
        };

        first.to_transaction(batchers, ctx, pk, sk)
    }
}

impl ExecuteChangesOnDatabase for InternalChanges {
    fn to_transaction<'a>(
        self,
        batchers: &'a DynamoDBBatchersData,
        ctx: &'a DynamoDBContext,
        pk: String,
        sk: String,
    ) -> ToTransactionFuture<'a> {
        match self {
            Self::Node(a) => a.to_transaction(batchers, ctx, pk, sk),
            Self::Relation(a) => a.to_transaction(batchers, ctx, pk, sk),
            Self::NodeConstraints(a) => a.to_transaction(batchers, ctx, pk, sk),
        }
    }
}
