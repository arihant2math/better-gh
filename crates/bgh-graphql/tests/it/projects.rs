//! Projects (v2): queries and mutations as `gh project` and
//! actions/add-to-project send them.

use bgh_core::testing::TestApp;
use serde_json::{Value, json};

use crate::common::{self, data, gql};

/// gh's fragment for a field (`queries.ProjectField`).
const FIELD: &str = "__typename,... on ProjectV2Field{id,name,dataType},\
    ... on ProjectV2IterationField{id,name,dataType},\
    ... on ProjectV2SingleSelectField{id,name,dataType,options{id,name}}";

/// gh's `queries.ProjectItem` selection.
fn item_selection() -> String {
    format!(
        "content{{__typename,... on DraftIssue{{id,body,title}},\
         ... on PullRequest{{body,title,number,url,repository{{nameWithOwner}}}},\
         ... on Issue{{body,title,number,url,repository{{nameWithOwner}}}}}},id,\
         fieldValues(first: 100){{nodes{{__typename,\
         ... on ProjectV2ItemFieldDateValue{{date,field{{{FIELD}}}}},\
         ... on ProjectV2ItemFieldIterationValue{{title,startDate,duration,field{{{FIELD}}},iterationId}},\
         ... on ProjectV2ItemFieldLabelValue{{labels(first: 10){{nodes{{name}}}},field{{{FIELD}}}}},\
         ... on ProjectV2ItemFieldNumberValue{{number,field{{{FIELD}}}}},\
         ... on ProjectV2ItemFieldSingleSelectValue{{name,field{{{FIELD}}}}},\
         ... on ProjectV2ItemFieldTextValue{{text,field{{{FIELD}}}}},\
         ... on ProjectV2ItemFieldMilestoneValue{{milestone{{title,description,dueOn}},field{{{FIELD}}}}},\
         ... on ProjectV2ItemFieldPullRequestValue{{pullRequests(first:10){{nodes{{url}}}},field{{{FIELD}}}}},\
         ... on ProjectV2ItemFieldRepositoryValue{{repository{{url}},field{{{FIELD}}}}},\
         ... on ProjectV2ItemFieldUserValue{{users(first: 10){{nodes{{login}}}},field{{{FIELD}}}}},\
         ... on ProjectV2ItemFieldReviewerValue{{reviewers(first: 10){{nodes{{__typename,... on Team{{name}},... on User{{login}}}}}},field{{{FIELD}}}}}}}}}"
    )
}

/// gh's `projectQueryWithoutQueryableItems` selection.
fn project_selection() -> String {
    format!(
        "number,url,shortDescription,public,closed,title,id,readme,\
         owner{{__typename,... on User{{login}},... on Organization{{login}}}},\
         fields(first: $firstFields, after: $afterFields){{totalCount,nodes{{{FIELD}}},pageInfo{{endCursor,hasNextPage}}}},\
         items(first: $firstItems, after: $afterItems){{pageInfo{{endCursor,hasNextPage}},totalCount,nodes{{{}}}}}",
        item_selection()
    )
}

const MUTATION_PROJECT: &str = "number,url,shortDescription,public,closed,title,id,readme,\
    items(first: $firstItems, after: $afterItems){totalCount},\
    fields(first: $firstFields, after: $afterFields){totalCount},\
    owner{__typename,... on User{login},... on Organization{login}}";

const PAGE_VARS: &str = "$afterFields:String$afterItems:String$firstFields:Int!$firstItems:Int!";

fn page_vars(first_fields: i64, first_items: i64) -> Value {
    json!({"firstFields": first_fields, "afterFields": null, "firstItems": first_items, "afterItems": null})
}

fn merge(mut a: Value, b: Value) -> Value {
    for (k, v) in b.as_object().unwrap() {
        a[k] = v.clone();
    }
    a
}

async fn create_issue(
    app: &TestApp,
    user: &bgh_core::testing::TestUser,
    nwo: &str,
    title: &str,
) -> Value {
    let res = app
        .post(&format!("/api/v3/repos/{nwo}/issues"))
        .auth(user)
        .json(&json!({"title": title, "labels": ["bug"]}))
        .send()
        .await;
    res.assert_status(201);
    res.json()
}

#[tokio::test]
async fn gh_project_flow() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_repo(&alice, "web").await;
    let issue = create_issue(&app, &alice, "alice/web", "Crash on start").await;

    // gh: OwnerIDAndType (user + organization in one query; one NOT_FOUND).
    let body = gql(
        &app,
        &alice,
        "query UserOrgOwner($login:String!){user(login: $login){login,id},organization(login: $login){login,id}}",
        json!({"login": "alice"}),
    )
    .await;
    assert_eq!(body["data"]["user"]["login"], "alice");
    assert_eq!(body["errors"][0]["type"], "NOT_FOUND");
    assert_eq!(body["errors"][0]["path"][0], "organization");
    let owner_id = body["data"]["user"]["id"].clone();

    // gh project create
    let d = data(
        &app,
        &alice,
        &format!(
            "mutation CreateProjectV2({PAGE_VARS}$input:CreateProjectV2Input!){{createProjectV2(input:$input){{projectV2{{{MUTATION_PROJECT}}}}}}}"
        ),
        merge(
            page_vars(0, 0),
            json!({"input": {"ownerId": owner_id, "title": "Roadmap"}}),
        ),
    )
    .await;
    let p = &d["createProjectV2"]["projectV2"];
    assert_eq!(p["number"], 1);
    assert_eq!(p["title"], "Roadmap");
    assert_eq!(p["owner"]["__typename"], "User");
    assert_eq!(p["owner"]["login"], "alice");
    assert_eq!(p["fields"]["totalCount"], 6);
    assert_eq!(p["items"]["totalCount"], 0);
    assert_eq!(p["url"], app.url("/users/alice/projects/1"));
    let project_id = p["id"].clone();

    // gh project list
    let d = data(
        &app,
        &alice,
        &format!(
            "query UserProjects($after:String{PAGE_VARS}$first:Int!$login:String!){{user(login: $login){{projectsV2(first: $first, after: $after){{totalCount,pageInfo{{endCursor,hasNextPage}},nodes{{{}}}}},login}}}}",
            project_selection()
        ),
        merge(page_vars(0, 0), json!({"login": "alice", "first": 30, "after": null})),
    )
    .await;
    assert_eq!(d["user"]["projectsV2"]["totalCount"], 1);
    assert_eq!(d["user"]["projectsV2"]["nodes"][0]["title"], "Roadmap");

    // gh project item-add (by URL: resource(url:) then addProjectV2ItemById)
    let d = data(
        &app,
        &alice,
        "query GetIssueOrPullRequest($url:URI!){resource(url: $url){__typename,... on Issue{id},... on PullRequest{id}}}",
        json!({"url": issue["html_url"]}),
    )
    .await;
    assert_eq!(d["resource"]["__typename"], "Issue");
    let content_id = d["resource"]["id"].clone();
    assert_eq!(content_id, issue["node_id"]);
    let d = data(
        &app,
        &alice,
        &format!(
            "mutation AddItem($input:AddProjectV2ItemByIdInput!){{addProjectV2ItemById(input:$input){{item{{{}}}}}}}",
            item_selection()
        ),
        json!({"input": {"projectId": project_id, "contentId": content_id}}),
    )
    .await;
    let item = &d["addProjectV2ItemById"]["item"];
    assert_eq!(item["content"]["__typename"], "Issue");
    assert_eq!(item["content"]["repository"]["nameWithOwner"], "alice/web");
    let item_id = item["id"].clone();
    let values = item["fieldValues"]["nodes"].as_array().unwrap();
    let kinds: Vec<&str> = values
        .iter()
        .map(|v| v["__typename"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        [
            "ProjectV2ItemFieldTextValue",
            "ProjectV2ItemFieldLabelValue",
            "ProjectV2ItemFieldRepositoryValue"
        ]
    );
    assert_eq!(values[0]["text"], "Crash on start");
    assert_eq!(values[0]["field"]["name"], "Title");
    assert_eq!(values[1]["labels"]["nodes"][0]["name"], "bug");

    // gh project item-create (draft)
    let d = data(
        &app,
        &alice,
        "mutation CreateDraftItem($input:AddProjectV2DraftIssueInput!){addProjectV2DraftIssue(input:$input){projectItem{id,content{__typename,... on DraftIssue{id,body,title}}}}}",
        json!({"input": {"projectId": project_id, "title": "Plan", "body": "later"}}),
    )
    .await;
    let draft = &d["addProjectV2DraftIssue"]["projectItem"];
    let draft_content_id = draft["content"]["id"].as_str().unwrap().to_string();
    assert!(draft_content_id.starts_with("DI_"));

    // gh project field-list
    let d = data(
        &app,
        &alice,
        &format!(
            "query UserProjectWithFields({PAGE_VARS}$login:String!$number:Int!){{user(login: $login){{projectV2(number: $number){{{}}}}}}}",
            project_selection()
        ),
        merge(page_vars(100, 0), json!({"login": "alice", "number": 1})),
    )
    .await;
    let fields = d["user"]["projectV2"]["fields"]["nodes"]
        .as_array()
        .unwrap()
        .clone();
    let status = fields.iter().find(|f| f["name"] == "Status").unwrap();
    assert_eq!(status["__typename"], "ProjectV2SingleSelectField");
    assert_eq!(status["dataType"], "SINGLE_SELECT");
    let done = status["options"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["name"] == "Done")
        .unwrap()["id"]
        .clone();

    // gh project field-create
    let d = data(
        &app,
        &alice,
        &format!("mutation CreateField($input:CreateProjectV2FieldInput!){{createProjectV2Field(input:$input){{projectV2Field{{{FIELD}}}}}}}"),
        json!({"input": {"projectId": project_id, "dataType": "SINGLE_SELECT", "name": "Priority",
                         "singleSelectOptions": [{"name": "High", "color": "GRAY", "description": ""}, {"name": "Low", "color": "GRAY", "description": ""}]}}),
    )
    .await;
    let priority = &d["createProjectV2Field"]["projectV2Field"];
    assert_eq!(priority["__typename"], "ProjectV2SingleSelectField");
    assert_eq!(priority["options"][1]["name"], "Low");
    let d = data(
        &app,
        &alice,
        &format!("mutation CreateField($input:CreateProjectV2FieldInput!){{createProjectV2Field(input:$input){{projectV2Field{{{FIELD}}}}}}}"),
        json!({"input": {"projectId": project_id, "dataType": "NUMBER", "name": "Points"}}),
    )
    .await;
    let points_id = d["createProjectV2Field"]["projectV2Field"]["id"].clone();
    let d = data(
        &app,
        &alice,
        &format!("mutation CreateField($input:CreateProjectV2FieldInput!){{createProjectV2Field(input:$input){{projectV2Field{{{FIELD}}}}}}}"),
        json!({"input": {"projectId": project_id, "dataType": "ITERATION", "name": "Sprint",
                         "iterationConfiguration": {"startDate": "2030-01-07", "duration": 14}}}),
    )
    .await;
    assert_eq!(
        d["createProjectV2Field"]["projectV2Field"]["__typename"],
        "ProjectV2IterationField"
    );

    // gh project item-edit (single select, number, clear)
    let edit = format!(
        "mutation UpdateItemValues($input:UpdateProjectV2ItemFieldValueInput!){{updateProjectV2ItemFieldValue(input:$input){{projectV2Item{{{}}}}}}}",
        item_selection()
    );
    let d = data(
        &app,
        &alice,
        &edit,
        json!({"input": {"projectId": project_id, "itemId": item_id, "fieldId": status["id"],
                         "value": {"singleSelectOptionId": done}}}),
    )
    .await;
    let values = d["updateProjectV2ItemFieldValue"]["projectV2Item"]["fieldValues"]["nodes"]
        .as_array()
        .unwrap()
        .clone();
    assert!(
        values.iter().any(
            |v| v["__typename"] == "ProjectV2ItemFieldSingleSelectValue" && v["name"] == "Done"
        )
    );
    let d = data(
        &app,
        &alice,
        &edit,
        json!({"input": {"projectId": project_id, "itemId": item_id, "fieldId": points_id,
                         "value": {"number": 5.0}}}),
    )
    .await;
    assert!(
        d["updateProjectV2ItemFieldValue"]["projectV2Item"]["fieldValues"]["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["number"] == 5.0)
    );
    // Wrong value type for the field.
    let body = gql(
        &app,
        &alice,
        &edit,
        json!({"input": {"projectId": project_id, "itemId": item_id, "fieldId": points_id,
                         "value": {"text": "nope"}}}),
    )
    .await;
    assert_eq!(body["errors"][0]["type"], "UNPROCESSABLE");
    let d = data(
        &app,
        &alice,
        "mutation ClearItemFieldValue($input:ClearProjectV2ItemFieldValueInput!){clearProjectV2ItemFieldValue(input:$input){projectV2Item{id}}}",
        json!({"input": {"projectId": project_id, "itemId": item_id, "fieldId": points_id}}),
    )
    .await;
    assert_eq!(
        d["clearProjectV2ItemFieldValue"]["projectV2Item"]["id"],
        item_id
    );

    // gh project item-edit for drafts (node lookup + updateProjectV2DraftIssue)
    let d = data(
        &app,
        &alice,
        "query DraftIssueByID($id:ID!){node(id: $id){... on DraftIssue{id,body,title}}}",
        json!({"id": draft_content_id}),
    )
    .await;
    assert_eq!(d["node"]["title"], "Plan");
    let d = data(
        &app,
        &alice,
        "mutation EditDraftIssueItem($input:UpdateProjectV2DraftIssueInput!){updateProjectV2DraftIssue(input:$input){draftIssue{id,body,title}}}",
        json!({"input": {"draftIssueId": draft_content_id, "title": "Plan v2", "body": "now"}}),
    )
    .await;
    assert_eq!(
        d["updateProjectV2DraftIssue"]["draftIssue"]["title"],
        "Plan v2"
    );

    // gh project item-list (with fieldValueByName as gh issue view uses)
    let d = data(
        &app,
        &alice,
        &format!(
            "query UserProjectWithItems({PAGE_VARS}$login:String!$number:Int!){{user(login: $login){{projectV2(number: $number){{{}}}}}}}",
            project_selection()
        ),
        merge(page_vars(100, 30), json!({"login": "alice", "number": 1})),
    )
    .await;
    let items = &d["user"]["projectV2"]["items"];
    assert_eq!(items["totalCount"], 2);
    assert_eq!(items["nodes"][1]["content"]["title"], "Plan v2");

    // gh issue view: projectItems with the Status value
    let d = data(
        &app,
        &alice,
        r#"query IssueProjectItems($endCursor:String$name:String!$number:Int!$owner:String!){repository(owner: $owner, name: $name){issue(number: $number){projectItems(first: 100, after: $endCursor){totalCount,nodes{id,project{id,title},status:fieldValueByName(name: "Status"){__typename,... on ProjectV2ItemFieldSingleSelectValue{optionId,name}}},pageInfo{hasNextPage,endCursor}}}}}"#,
        json!({"owner": "alice", "name": "web", "number": issue["number"], "endCursor": null}),
    )
    .await;
    let pi = &d["repository"]["issue"]["projectItems"];
    assert_eq!(pi["totalCount"], 1);
    assert_eq!(pi["nodes"][0]["project"]["title"], "Roadmap");
    assert_eq!(pi["nodes"][0]["status"]["name"], "Done");

    // gh project item-archive (and --undo)
    let d = data(
        &app,
        &alice,
        "mutation ArchiveProjectItem($input:ArchiveProjectV2ItemInput!){archiveProjectV2Item(input:$input){item{id}}}",
        json!({"input": {"projectId": project_id, "itemId": item_id}}),
    )
    .await;
    assert_eq!(d["archiveProjectV2Item"]["item"]["id"], item_id);
    let body = gql(
        &app,
        &alice,
        "query($id:ID!){node(id:$id){... on ProjectV2Item{isArchived}}}",
        json!({"id": item_id}),
    )
    .await;
    assert_eq!(body["data"]["node"]["isArchived"], true);
    let d = data(
        &app,
        &alice,
        "query($login:String!){user(login:$login){projectV2(number:1){items(first:10){totalCount} archived: items(first:10, query:\"is:archived\"){totalCount}}}}",
        json!({"login": "alice"}),
    )
    .await;
    assert_eq!(d["user"]["projectV2"]["items"]["totalCount"], 1);
    assert_eq!(d["user"]["projectV2"]["archived"]["totalCount"], 1);
    data(
        &app,
        &alice,
        "mutation UnarchiveProjectItem($input:UnarchiveProjectV2ItemInput!){unarchiveProjectV2Item(input:$input){item{id}}}",
        json!({"input": {"projectId": project_id, "itemId": item_id}}),
    )
    .await;

    // Position and delete.
    let d = data(
        &app,
        &alice,
        "mutation($input:UpdateProjectV2ItemPositionInput!){updateProjectV2ItemPosition(input:$input){items{nodes{id}}}}",
        json!({"input": {"projectId": project_id, "itemId": item_id, "afterId": draft["id"]}}),
    )
    .await;
    let order: Vec<Value> = d["updateProjectV2ItemPosition"]["items"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].clone())
        .collect();
    assert_eq!(order, vec![draft["id"].clone(), item_id.clone()]);
    let d = data(
        &app,
        &alice,
        "mutation DeleteProjectItem($input:DeleteProjectV2ItemInput!){deleteProjectV2Item(input:$input){deletedItemId}}",
        json!({"input": {"projectId": project_id, "itemId": draft["id"]}}),
    )
    .await;
    assert_eq!(d["deleteProjectV2Item"]["deletedItemId"], draft["id"]);

    // gh project close / edit
    let d = data(
        &app,
        &alice,
        &format!("mutation CloseProjectV2({PAGE_VARS}$input:UpdateProjectV2Input!){{updateProjectV2(input:$input){{projectV2{{{MUTATION_PROJECT}}}}}}}"),
        merge(page_vars(0, 0), json!({"input": {"projectId": project_id, "closed": true, "shortDescription": "desc"}})),
    )
    .await;
    assert_eq!(d["updateProjectV2"]["projectV2"]["closed"], true);
    assert_eq!(
        d["updateProjectV2"]["projectV2"]["shortDescription"],
        "desc"
    );
    let d = data(
        &app,
        &alice,
        "query{viewer{open: projectsV2(first:10, query:\"is:open\"){totalCount} closed: projectsV2(first:10, query:\"is:closed\"){totalCount}}}",
        json!({}),
    )
    .await;
    assert_eq!(d["viewer"]["open"]["totalCount"], 0);
    assert_eq!(d["viewer"]["closed"]["totalCount"], 1);

    // gh project field-delete
    let d = data(
        &app,
        &alice,
        &format!("mutation DeleteField($input:DeleteProjectV2FieldInput!){{deleteProjectV2Field(input:$input){{projectV2Field{{{FIELD}}}}}}}"),
        json!({"input": {"fieldId": points_id}}),
    )
    .await;
    assert_eq!(
        d["deleteProjectV2Field"]["projectV2Field"]["name"],
        "Points"
    );

    // gh project delete
    let d = data(
        &app,
        &alice,
        &format!("mutation DeleteProject({PAGE_VARS}$input:DeleteProjectV2Input!){{deleteProjectV2(input:$input){{projectV2{{{MUTATION_PROJECT}}}}}}}"),
        merge(page_vars(0, 0), json!({"input": {"projectId": project_id}})),
    )
    .await;
    assert_eq!(d["deleteProjectV2"]["projectV2"]["title"], "Roadmap");
    let body = gql(
        &app,
        &alice,
        "query{user(login:\"alice\"){projectV2(number:1){id}}}",
        json!({}),
    )
    .await;
    assert_eq!(body["errors"][0]["type"], "NOT_FOUND");
}

/// The queries and mutations of actions/add-to-project.
#[tokio::test]
async fn add_to_project_action_fixture() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let org = app.create_org("acme", &alice).await;
    app.create_repo_with(&alice, Some("acme"), json!({"name": "web"}))
        .await;
    let issue = create_issue(&app, &alice, "acme/web", "From the action").await;
    let p = app
        .post("/_bgh/projects")
        .auth(&alice)
        .json(&json!({"owner": org.login, "title": "Triage"}))
        .send()
        .await;
    p.assert_status(201);

    let d = data(
        &app,
        &alice,
        "query getProject($projectOwnerName: String!, $projectNumber: Int!) {
           organization(login: $projectOwnerName) { projectV2(number: $projectNumber) { id } }
         }",
        json!({"projectOwnerName": "acme", "projectNumber": 1}),
    )
    .await;
    let project_id = d["organization"]["projectV2"]["id"].clone();
    let d = data(
        &app,
        &alice,
        "mutation addIssueToProject($input: AddProjectV2ItemByIdInput!) {
           addProjectV2ItemById(input: $input) { item { id } }
         }",
        json!({"input": {"projectId": project_id, "contentId": issue["node_id"]}}),
    )
    .await;
    let item = d["addProjectV2ItemById"]["item"]["id"].clone();
    assert!(item.is_string());
    // Adding again is idempotent (same item).
    let d = data(
        &app,
        &alice,
        "mutation addIssueToProject($input: AddProjectV2ItemByIdInput!) {
           addProjectV2ItemById(input: $input) { item { id } }
         }",
        json!({"input": {"projectId": project_id, "contentId": issue["node_id"]}}),
    )
    .await;
    assert_eq!(d["addProjectV2ItemById"]["item"]["id"], item);
    let d = data(
        &app,
        &alice,
        "mutation addDraftIssueToProject($projectId: ID!, $title: String!) {
           addProjectV2DraftIssue(input: {projectId: $projectId, title: $title}) { projectItem { id } }
         }",
        json!({"projectId": project_id, "title": "Cross-org issue"}),
    )
    .await;
    assert!(d["addProjectV2DraftIssue"]["projectItem"]["id"].is_string());

    // The org's and the repo's projectsV2 now list it; node() resolves it.
    let d = data(
        &app,
        &alice,
        r#"query { organization(login: "acme") { viewerCanCreateProjects projectsV2(first: 100, orderBy: {field: TITLE, direction: ASC}, query: "is:open") { nodes { id title number resourcePath closed url } } }
                   repository(owner: "acme", name: "web") { projectsV2(first: 100, orderBy: {field: TITLE, direction: ASC}, query: "is:open") { totalCount } } }"#,
        json!({}),
    )
    .await;
    assert_eq!(d["organization"]["viewerCanCreateProjects"], true);
    let node = &d["organization"]["projectsV2"]["nodes"][0];
    assert_eq!(node["title"], "Triage");
    assert_eq!(node["resourcePath"], "/orgs/acme/projects/1");
    assert_eq!(d["repository"]["projectsV2"]["totalCount"], 1);
    let d = data(
        &app,
        &alice,
        "query($id:ID!){node(id:$id){__typename ... on ProjectV2{title viewerCanUpdate}}}",
        json!({"id": project_id}),
    )
    .await;
    assert_eq!(d["node"]["__typename"], "ProjectV2");
    assert_eq!(d["node"]["viewerCanUpdate"], true);
}

#[tokio::test]
async fn create_issue_with_project_v2_ids_and_links() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let org = app.create_org("acme", &alice).await;
    let repo = app
        .create_repo_with(&alice, Some("acme"), json!({"name": "web"}))
        .await;
    let res = app
        .post("/_bgh/projects")
        .auth(&alice)
        .json(&json!({"owner": org.login, "title": "Board"}))
        .send()
        .await;
    let pid = res.json()["id"].as_i64().unwrap();
    let project_id = bgh_core::node_id::encode(bgh_core::node_id::NodeType::ProjectV2, pid);

    // gh issue create --project: createIssue, then addProjectV2ItemById
    // (gh) or projectV2Ids (other clients).
    let d = data(
        &app,
        &alice,
        "mutation($input:CreateIssueInput!){createIssue(input:$input){issue{id projectItems(first:10){nodes{project{title}}} projectsV2(first:5){totalCount}}}}",
        json!({"input": {"repositoryId": repo["node_id"], "title": "Tracked", "projectV2Ids": [project_id]}}),
    )
    .await;
    let issue = &d["createIssue"]["issue"];
    assert_eq!(
        issue["projectItems"]["nodes"][0]["project"]["title"],
        "Board"
    );
    assert_eq!(issue["projectsV2"]["totalCount"], 1);

    // Unknown project ids fail before the issue's items are written.
    let body = gql(
        &app,
        &alice,
        "mutation($input:CreateIssueInput!){createIssue(input:$input){issue{id}}}",
        json!({"input": {"repositoryId": repo["node_id"], "title": "x", "projectV2Ids": ["bogus"]}}),
    )
    .await;
    assert_eq!(body["errors"][0]["type"], "NOT_FOUND");

    // Link to repository and team.
    let team = app
        .post("/api/v3/orgs/acme/teams")
        .auth(&alice)
        .json(&json!({"name": "core"}))
        .send()
        .await
        .json();
    data(
        &app,
        &alice,
        "mutation($input:LinkProjectV2ToRepositoryInput!){linkProjectV2ToRepository(input:$input){repository{name}}}",
        json!({"input": {"projectId": project_id, "repositoryId": repo["node_id"]}}),
    )
    .await;
    data(
        &app,
        &alice,
        "mutation($input:LinkProjectV2ToTeamInput!){linkProjectV2ToTeam(input:$input){team{name}}}",
        json!({"input": {"projectId": project_id, "teamId": team["node_id"]}}),
    )
    .await;
    let d = data(
        &app,
        &alice,
        "query($id:ID!){node(id:$id){... on ProjectV2{repositories(first:5){nodes{name}} teams(first:5){nodes{name}} views(first:5){nodes{name layout number}}}}}",
        json!({"id": project_id}),
    )
    .await;
    assert_eq!(d["node"]["repositories"]["nodes"][0]["name"], "web");
    assert_eq!(d["node"]["teams"]["nodes"][0]["name"], "core");
    assert_eq!(d["node"]["views"]["nodes"][0]["layout"], "TABLE_LAYOUT");
    data(
        &app,
        &alice,
        "mutation($input:UnlinkProjectV2FromTeamInput!){unlinkProjectV2FromTeam(input:$input){team{name}}}",
        json!({"input": {"projectId": project_id, "teamId": team["node_id"]}}),
    )
    .await;
    data(
        &app,
        &alice,
        "mutation($input:UnlinkProjectV2FromRepositoryInput!){unlinkProjectV2FromRepository(input:$input){repository{name}}}",
        json!({"input": {"projectId": project_id, "repositoryId": repo["node_id"]}}),
    )
    .await;

    // Outsiders can't see the private project.
    let body = common::gql(
        &app,
        &bob,
        "query($id:ID!){node(id:$id){id}}",
        json!({"id": project_id}),
    )
    .await;
    assert_eq!(body["errors"][0]["type"], "NOT_FOUND");
    let body = common::gql(
        &app,
        &bob,
        "mutation($input:UpdateProjectV2Input!){updateProjectV2(input:$input){projectV2{id}}}",
        json!({"input": {"projectId": project_id, "title": "pwned"}}),
    )
    .await;
    assert_eq!(body["errors"][0]["type"], "NOT_FOUND");
    let d = data(
        &app,
        &bob,
        r#"query{organization(login:"acme"){projectsV2(first:5){totalCount}}}"#,
        json!({}),
    )
    .await;
    assert_eq!(d["organization"]["projectsV2"]["totalCount"], 0);
}

#[test]
fn schema_has_project_types() {
    let sdl = bgh_graphql::sdl();
    for t in [
        "type ProjectV2 ",
        "type ProjectV2Item ",
        "union ProjectV2FieldConfiguration",
        "union ProjectV2ItemFieldValue",
        "union ProjectV2ItemContent",
        "interface ProjectV2Owner",
        "type DraftIssue ",
        "input ProjectV2FieldValue",
        "enum ProjectV2CustomFieldType",
        "scalar Date",
        "scalar URI",
        "scalar HTML",
        "resource(url: URI!)",
        "addProjectV2ItemById(",
        "updateProjectV2ItemFieldValue(",
    ] {
        assert!(sdl.contains(t), "missing {t}");
    }
}
